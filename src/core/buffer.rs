//! The piece table. A document is a sequence of pieces, each a byte range of one immutable source. Opening a
//! file makes a single piece; edits split pieces and add new ones that point at typed or pasted text (kept in
//! memory). Pieces are grouped in leaves of at most `LEAF_MAX` with cached byte and newline totals, so lookups by
//! offset or by line stay fast even after millions of edits, and nothing ever copies the original file.

use std::sync::Arc;

use super::source::Source;

pub const LEAF_MAX: usize = 256;
/// Inserts at least this big get a source of their own instead of growing the add buffer.
const BIG_INSERT: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub src: u32,
    pub start: u64,
    pub len: u64,
    pub nl: u64,
}

#[derive(Clone, Default)]
struct Leaf {
    pieces: Vec<Piece>,
    len: u64,
    nl: u64,
}

impl Leaf {
    fn resum(&mut self) {
        self.len = self.pieces.iter().map(|p| p.len).sum();
        self.nl = self.pieces.iter().map(|p| p.nl).sum();
    }
}

/// Where a piece sits: leaf, index in the leaf, and the document offset / newline count before it.
#[derive(Clone, Copy, Debug)]
struct Loc {
    leaf: usize,
    idx: usize,
    off: u64,
    nl: u64,
}

pub struct Buffer {
    sources: Vec<Arc<Source>>,
    /// The open add buffer. Its pieces use `src == add_id`; `sources[add_id]` is a placeholder until it is frozen.
    add: Vec<u8>,
    add_id: u32,
    leaves: Vec<Leaf>,
    len: u64,
    nl: u64,
}

/// An immutable view of a buffer for background work (search, save, formatting).
#[derive(Clone)]
pub struct Snapshot {
    pieces: Arc<Vec<(u64, Piece)>>,
    sources: Vec<Arc<Source>>,
    len: u64,
}

fn placeholder() -> Arc<Source> {
    Arc::new(Source::from_vec(Vec::new()))
}

impl Buffer {
    pub fn new() -> Buffer {
        Buffer {
            sources: vec![placeholder()],
            add: Vec::new(),
            add_id: 0,
            leaves: vec![Leaf::default()],
            len: 0,
            nl: 0,
        }
    }

    /// A buffer holding all of `src`. `nl` is its newline count (0 while a file is still being indexed; call
    /// `recount` once the index is done).
    pub fn from_source(src: Arc<Source>, nl: u64) -> Buffer {
        let mut b = Buffer::new();
        let len = src.len();
        let id = b.push_source(src);
        if len > 0 {
            b.leaves[0].pieces.push(Piece { src: id, start: 0, len, nl });
            b.leaves[0].resum();
            b.retotal();
        }
        b
    }

    pub fn push_source(&mut self, src: Arc<Source>) -> u32 {
        self.sources.push(src);
        (self.sources.len() - 1) as u32
    }

    pub fn source(&self, id: u32) -> &Arc<Source> {
        &self.sources[id as usize]
    }

    pub fn sources(&self) -> &[Arc<Source>] {
        &self.sources
    }

    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Number of `\n` in the document.
    pub fn newlines(&self) -> u64 {
        self.nl
    }
    pub fn line_count(&self) -> u64 {
        self.nl + 1
    }
    pub fn piece_count(&self) -> usize {
        self.leaves.iter().map(|l| l.pieces.len()).sum()
    }

    /// Recomputes every piece's newline count (after a source finished indexing).
    pub fn recount(&mut self) {
        for li in 0..self.leaves.len() {
            for pi in 0..self.leaves[li].pieces.len() {
                let p = self.leaves[li].pieces[pi];
                self.leaves[li].pieces[pi].nl = self.src_count(p.src, p.start, p.start + p.len);
            }
            self.leaves[li].resum();
        }
        self.retotal();
    }

    fn retotal(&mut self) {
        self.len = self.leaves.iter().map(|l| l.len).sum();
        self.nl = self.leaves.iter().map(|l| l.nl).sum();
    }

    pub fn pieces(&self) -> impl Iterator<Item = &Piece> {
        self.leaves.iter().flat_map(|l| l.pieces.iter())
    }

    // ---- per-source helpers (the add buffer lives outside `sources` until frozen) ----

    fn src_chunks(&self, src: u32, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        if src == self.add_id {
            f(&self.add[a as usize..b as usize])
        } else {
            self.sources[src as usize].chunks(a, b, f)
        }
    }

    fn src_count(&self, src: u32, a: u64, b: u64) -> u64 {
        if src == self.add_id {
            bytecount::count(&self.add[a as usize..b as usize], b'\n') as u64
        } else {
            self.sources[src as usize].count_nl(a, b)
        }
    }

    fn src_nth(&self, src: u32, a: u64, b: u64, k: u64) -> Option<u64> {
        if src == self.add_id {
            memchr::memchr_iter(b'\n', &self.add[a as usize..b as usize]).nth(k as usize - 1).map(|i| a + i as u64)
        } else {
            self.sources[src as usize].nth_nl(a, b, k)
        }
    }

    fn src_fwd(&self, src: u32, a: u64, b: u64) -> Option<u64> {
        if src == self.add_id {
            memchr::memchr(b'\n', &self.add[a as usize..b as usize]).map(|i| a + i as u64)
        } else {
            self.sources[src as usize].find_nl_fwd(a, b)
        }
    }

    fn src_back(&self, src: u32, a: u64, b: u64) -> Option<u64> {
        if src == self.add_id {
            memchr::memrchr(b'\n', &self.add[a as usize..b as usize]).map(|i| a + i as u64)
        } else {
            self.sources[src as usize].find_nl_back(a, b)
        }
    }

    // ---- locating ----

    /// The piece containing `off` (for `off == len`, the end position).
    fn locate(&self, off: u64) -> Loc {
        let mut acc = 0u64;
        let mut nl = 0u64;
        for (li, leaf) in self.leaves.iter().enumerate() {
            if off < acc + leaf.len {
                for (pi, p) in leaf.pieces.iter().enumerate() {
                    if off < acc + p.len {
                        return Loc { leaf: li, idx: pi, off: acc, nl };
                    }
                    acc += p.len;
                    nl += p.nl;
                }
                unreachable!("leaf sums out of date");
            }
            acc += leaf.len;
            nl += leaf.nl;
        }
        let last = self.leaves.len() - 1;
        Loc { leaf: last, idx: self.leaves[last].pieces.len(), off: acc, nl }
    }

    /// The piece containing the `k`-th newline (1-based), if there is one.
    fn locate_nl(&self, k: u64) -> Option<Loc> {
        if k == 0 || k > self.nl {
            return None;
        }
        let mut acc = 0u64;
        let mut nl = 0u64;
        for (li, leaf) in self.leaves.iter().enumerate() {
            if nl + leaf.nl >= k {
                for (pi, p) in leaf.pieces.iter().enumerate() {
                    if nl + p.nl >= k {
                        return Some(Loc { leaf: li, idx: pi, off: acc, nl });
                    }
                    acc += p.len;
                    nl += p.nl;
                }
            }
            acc += leaf.len;
            nl += leaf.nl;
        }
        None
    }

    fn piece(&self, l: &Loc) -> Piece {
        self.leaves[l.leaf].pieces[l.idx]
    }

    /// Next piece position after `l` (skipping empty leaves), or None at the end.
    fn next_loc(&self, l: &Loc) -> Option<Loc> {
        let p = self.piece(l);
        let (mut leaf, mut idx) = (l.leaf, l.idx + 1);
        while idx >= self.leaves[leaf].pieces.len() {
            leaf += 1;
            idx = 0;
            if leaf >= self.leaves.len() {
                return None;
            }
        }
        Some(Loc { leaf, idx, off: l.off + p.len, nl: l.nl + p.nl })
    }

    fn prev_loc(&self, l: &Loc) -> Option<Loc> {
        let (mut leaf, mut idx) = (l.leaf, l.idx);
        loop {
            if idx > 0 {
                idx -= 1;
                break;
            }
            if leaf == 0 {
                return None;
            }
            leaf -= 1;
            idx = self.leaves[leaf].pieces.len();
        }
        let p = self.leaves[leaf].pieces[idx];
        Some(Loc { leaf, idx, off: l.off - p.len, nl: l.nl - p.nl })
    }

    // ---- reading ----

    /// Calls `f` with consecutive slices covering `[a, b)`; returns false if `f` stopped early.
    pub fn chunks(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        let b = b.min(self.len);
        if a >= b {
            return true;
        }
        let mut l = self.locate(a);
        loop {
            let p = self.piece(&l);
            let s = a.max(l.off) - l.off;
            let e = (b - l.off).min(p.len);
            if !self.src_chunks(p.src, p.start + s, p.start + e, f) {
                return false;
            }
            if l.off + p.len >= b {
                return true;
            }
            match self.next_loc(&l) {
                Some(n) => l = n,
                None => return true,
            }
        }
    }

    pub fn read_into(&self, a: u64, b: u64, out: &mut Vec<u8>) {
        self.chunks(a, b, &mut |c| {
            out.extend_from_slice(c);
            true
        });
    }

    pub fn read(&self, a: u64, b: u64) -> Vec<u8> {
        let mut v = Vec::with_capacity(b.saturating_sub(a).min(1 << 24) as usize);
        self.read_into(a, b, &mut v);
        v
    }

    pub fn byte_at(&self, pos: u64) -> Option<u8> {
        if pos >= self.len {
            return None;
        }
        let l = self.locate(pos);
        let p = self.piece(&l);
        let at = p.start + (pos - l.off);
        Some(if p.src == self.add_id { self.add[at as usize] } else { self.sources[p.src as usize].byte_at(at) })
    }

    // ---- lines ----

    /// Number of newlines before `off` (= the 0-based line of `off`).
    pub fn line_of(&self, off: u64) -> u64 {
        let off = off.min(self.len);
        if off == self.len {
            return self.nl;
        }
        let l = self.locate(off);
        let p = self.piece(&l);
        l.nl + self.src_count(p.src, p.start, p.start + (off - l.off))
    }

    /// Position of the `k`-th newline (1-based).
    pub fn nth_newline(&self, k: u64) -> Option<u64> {
        let l = self.locate_nl(k)?;
        let p = self.piece(&l);
        self.src_nth(p.src, p.start, p.start + p.len, k - l.nl).map(|pos| l.off + (pos - p.start))
    }

    /// Offset where 0-based `line` starts (clamped to the end).
    pub fn line_start(&self, line: u64) -> u64 {
        if line == 0 {
            return 0;
        }
        match self.nth_newline(line) {
            Some(p) => p + 1,
            None => self.len,
        }
    }

    /// First `\n` at or after `off`.
    pub fn next_newline(&self, off: u64) -> Option<u64> {
        if off >= self.len {
            return None;
        }
        let mut l = self.locate(off);
        let mut from = off;
        loop {
            let p = self.piece(&l);
            if p.nl > 0 {
                let s = p.start + (from - l.off);
                if let Some(pos) = self.src_fwd(p.src, s, p.start + p.len) {
                    return Some(l.off + (pos - p.start));
                }
            }
            l = self.next_loc(&l)?;
            from = l.off;
        }
    }

    /// Last `\n` before `off`.
    pub fn prev_newline(&self, off: u64) -> Option<u64> {
        let off = off.min(self.len);
        if off == 0 {
            return None;
        }
        let mut l = self.locate(off - 1);
        let mut to = off;
        loop {
            let p = self.piece(&l);
            if p.nl > 0 {
                let e = p.start + (to - l.off);
                if let Some(pos) = self.src_back(p.src, p.start, e) {
                    return Some(l.off + (pos - p.start));
                }
            }
            l = self.prev_loc(&l)?;
            to = l.off + self.piece(&l).len;
        }
    }

    /// Start of the line containing `off`.
    pub fn line_start_of(&self, off: u64) -> u64 {
        self.prev_newline(off).map_or(0, |p| p + 1)
    }

    /// End of the line's text containing `off`, before its `\r\n` / `\n`.
    pub fn line_end_of(&self, off: u64) -> u64 {
        // If the line is empty, the byte before its `\n` is the previous line's `\n`, never `\r`.
        match self.next_newline(off) {
            Some(p) if p > 0 && self.byte_at(p - 1) == Some(b'\r') => p - 1,
            Some(p) => p,
            None => self.len,
        }
    }

    // ---- editing ----

    /// Makes sure a piece starts at `off`; returns (leaf, index) of that piece (or the end position).
    fn split_at(&mut self, off: u64) -> (usize, usize) {
        let l = self.locate(off);
        if l.off == off || l.idx >= self.leaves[l.leaf].pieces.len() {
            return (l.leaf, l.idx);
        }
        let p = self.piece(&l);
        let k = off - l.off;
        let left_nl = self.src_count(p.src, p.start, p.start + k);
        let left = Piece { src: p.src, start: p.start, len: k, nl: left_nl };
        // (saturating: a stale index must not turn into a panic or a wrapped-around count)
        let right = Piece { src: p.src, start: p.start + k, len: p.len - k, nl: p.nl.saturating_sub(left_nl) };
        let leaf = &mut self.leaves[l.leaf];
        leaf.pieces[l.idx] = left;
        leaf.pieces.insert(l.idx + 1, right);
        (l.leaf, l.idx + 1)
    }

    /// Splits overfull leaves and drops empty ones (keeping at least one), then refreshes totals.
    fn rebalance(&mut self) {
        let mut out: Vec<Leaf> = Vec::with_capacity(self.leaves.len());
        for mut leaf in std::mem::take(&mut self.leaves) {
            if leaf.pieces.is_empty() {
                continue;
            }
            if leaf.pieces.len() > LEAF_MAX {
                for chunk in leaf.pieces.chunks(LEAF_MAX / 2) {
                    let mut l = Leaf { pieces: chunk.to_vec(), len: 0, nl: 0 };
                    l.resum();
                    out.push(l);
                }
            } else {
                leaf.resum();
                out.push(leaf);
            }
        }
        if out.is_empty() {
            out.push(Leaf::default());
        }
        self.leaves = out;
        self.retotal();
    }

    /// Like `rebalance` but only touches the given leaf (the common, cheap case).
    fn fix_leaf(&mut self, li: usize) {
        let n = self.leaves[li].pieces.len();
        if n > LEAF_MAX || (n == 0 && self.leaves.len() > 1) {
            self.rebalance();
        } else {
            self.leaves[li].resum();
            self.retotal();
        }
    }

    /// Inserts `pieces` at `off`.
    pub fn insert_pieces(&mut self, off: u64, pieces: &[Piece]) {
        if pieces.is_empty() {
            return;
        }
        let (li, idx) = self.split_at(off.min(self.len));
        self.leaves[li].pieces.splice(idx..idx, pieces.iter().copied());
        self.fix_leaf(li);
    }

    /// Inserts `text` at `off` and returns the pieces that now hold it (for undo).
    pub fn insert(&mut self, off: u64, text: &[u8]) -> Vec<Piece> {
        if text.is_empty() {
            return Vec::new();
        }
        let off = off.min(self.len);
        let nl = bytecount::count(text, b'\n') as u64;
        if text.len() >= BIG_INSERT {
            let id = self.push_source(Arc::new(Source::from_vec(text.to_vec())));
            let p = Piece { src: id, start: 0, len: text.len() as u64, nl };
            self.insert_pieces(off, &[p]);
            return vec![p];
        }
        let start = self.add.len() as u64;
        self.add.extend_from_slice(text);
        let new = Piece { src: self.add_id, start, len: text.len() as u64, nl };
        // Typing: extend the piece that ends right here if it is the tail of the add buffer.
        if off > 0 {
            let l = self.locate(off - 1);
            let p = self.piece(&l);
            if p.src == self.add_id && l.off + p.len == off && p.start + p.len == start {
                let leaf = &mut self.leaves[l.leaf];
                leaf.pieces[l.idx].len += new.len;
                leaf.pieces[l.idx].nl += nl;
                leaf.len += new.len;
                leaf.nl += nl;
                self.len += new.len;
                self.nl += nl;
                return vec![new];
            }
        }
        self.insert_pieces(off, &[new]);
        vec![new]
    }

    /// Deletes `[a, b)` and returns the removed pieces (for undo).
    pub fn delete(&mut self, a: u64, b: u64) -> Vec<Piece> {
        let b = b.min(self.len);
        if a >= b {
            return Vec::new();
        }
        let (l1, i1) = self.split_at(a);
        let (l2, i2) = self.split_at(b);
        let mut removed = Vec::new();
        if l1 == l2 {
            removed.extend(self.leaves[l1].pieces.drain(i1..i2));
            self.fix_leaf(l1);
        } else {
            removed.extend(self.leaves[l1].pieces.drain(i1..));
            for li in l1 + 1..l2 {
                removed.append(&mut self.leaves[li].pieces);
            }
            removed.extend(self.leaves[l2].pieces.drain(..i2));
            self.rebalance();
        }
        removed
    }

    /// Replaces the whole content with `pieces`, returning the old pieces.
    pub fn replace_all(&mut self, pieces: &[Piece]) -> Vec<Piece> {
        let old: Vec<Piece> = self.pieces().copied().collect();
        self.leaves = vec![Leaf { pieces: pieces.to_vec(), len: 0, nl: 0 }];
        self.rebalance();
        old
    }

    /// Freezes the add buffer so the current content can be shared with other threads.
    pub fn snapshot(&mut self) -> Snapshot {
        if !self.add.is_empty() {
            let data = std::mem::take(&mut self.add);
            self.sources[self.add_id as usize] = Arc::new(Source::from_vec(data));
            self.sources.push(placeholder());
            self.add_id = (self.sources.len() - 1) as u32;
        }
        let mut pieces = Vec::with_capacity(self.piece_count());
        let mut off = 0;
        for p in self.pieces() {
            pieces.push((off, *p));
            off += p.len;
        }
        Snapshot { pieces: Arc::new(pieces), sources: self.sources.clone(), len: self.len }
    }

    /// Lets go of sources that neither the current text nor `also` (pieces kept elsewhere, e.g. undo history)
    /// uses, so an old version of a big file isn't held open. Their ids stay valid (empty placeholders).
    pub fn release_unused(&mut self, also: &[u32]) {
        let mut used = vec![false; self.sources.len()];
        for p in self.pieces() {
            used[p.src as usize] = true;
        }
        for &id in also {
            if let Some(u) = used.get_mut(id as usize) {
                *u = true;
            }
        }
        used[self.add_id as usize] = true;
        for (i, u) in used.iter().enumerate() {
            if !u && !self.sources[i].is_empty() {
                self.sources[i] = placeholder();
            }
        }
    }

    /// Total read errors of the sources the current content uses (see `Source::read_errors`).
    pub fn read_errors(&self) -> u64 {
        let mut used: Vec<u32> = self.pieces().map(|p| p.src).collect();
        used.sort_unstable();
        used.dedup();
        used.iter().filter(|&&s| s != self.add_id).map(|&s| self.sources[s as usize].read_errors()).sum()
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshot {
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn sources(&self) -> &[Arc<Source>] {
        &self.sources
    }

    /// Calls `f` with consecutive slices covering `[a, b)`; returns false if `f` stopped early.
    pub fn chunks(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        let b = b.min(self.len);
        if a >= b {
            return true;
        }
        let mut i = self.pieces.partition_point(|(off, p)| off + p.len <= a);
        while i < self.pieces.len() {
            let (off, p) = self.pieces[i];
            if off >= b {
                break;
            }
            let s = a.max(off) - off;
            let e = (b - off).min(p.len);
            if !self.sources[p.src as usize].chunks(p.start + s, p.start + e, f) {
                return false;
            }
            i += 1;
        }
        true
    }

    pub fn read_into(&self, a: u64, b: u64, out: &mut Vec<u8>) {
        self.chunks(a, b, &mut |c| {
            out.extend_from_slice(c);
            true
        });
    }

    /// The bytes of `[a, b)` without copying when they sit in one in-memory piece.
    pub fn slice<'a>(&'a self, a: u64, b: u64, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        let b = b.min(self.len);
        if a >= b {
            return &[];
        }
        let i = self.pieces.partition_point(|(off, p)| off + p.len <= a);
        let (off, p) = self.pieces[i];
        if b <= off + p.len {
            if let Some(m) = self.sources[p.src as usize].mem() {
                let s = (p.start + a - off) as usize;
                return &m[s..s + (b - a) as usize];
            }
        }
        scratch.clear();
        self.read_into(a, b, scratch);
        scratch
    }

    /// Total read errors of the sources this snapshot uses.
    pub fn read_errors(&self) -> u64 {
        let mut used: Vec<u32> = self.pieces.iter().map(|(_, p)| p.src).collect();
        used.sort_unstable();
        used.dedup();
        used.iter().map(|&s| self.sources[s as usize].read_errors()).sum()
    }

    /// The pieces with their document offsets.
    pub fn pieces(&self) -> &[(u64, Piece)] {
        &self.pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            if n == 0 { 0 } else { self.next() % n }
        }
    }

    fn text(r: &mut Rng, n: usize) -> Vec<u8> {
        (0..n).map(|_| if r.below(8) == 0 { b'\n' } else { b'a' + r.below(26) as u8 }).collect()
    }

    fn check(b: &Buffer, model: &[u8]) {
        assert_eq!(b.len(), model.len() as u64);
        assert_eq!(b.read(0, b.len()), model);
        let nls: Vec<u64> = model.iter().enumerate().filter(|(_, c)| **c == b'\n').map(|(i, _)| i as u64).collect();
        assert_eq!(b.newlines(), nls.len() as u64);
        for (k, &p) in nls.iter().enumerate().step_by(7) {
            assert_eq!(b.nth_newline(k as u64 + 1), Some(p));
            assert_eq!(b.line_of(p), k as u64);
            assert_eq!(b.line_of(p + 1), k as u64 + 1);
            assert_eq!(b.line_start(k as u64 + 1), p + 1);
        }
        for off in (0..=model.len() as u64).step_by(13) {
            let next = nls.iter().copied().find(|&p| p >= off);
            let prev = nls.iter().copied().filter(|&p| p < off).last();
            assert_eq!(b.next_newline(off), next, "next_newline({off})");
            assert_eq!(b.prev_newline(off), prev, "prev_newline({off})");
        }
    }

    #[test]
    fn random_edits_match_model() {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15);
        let base = text(&mut r, 300_000);
        let mut b = Buffer::from_source(Arc::new(Source::from_vec(base.clone())), bytecount::count(&base, b'\n') as u64);
        let mut model = base;
        for step in 0..3000 {
            let len = model.len() as u64;
            match r.below(10) {
                0..=4 => {
                    let at = r.below(len + 1);
                    let n = 1 + r.below(20) as usize;
                    let t = text(&mut r, n);
                    b.insert(at, &t);
                    model.splice(at as usize..at as usize, t);
                }
                5..=7 => {
                    let a = r.below(len + 1);
                    let e = (a + r.below(200)).min(len);
                    b.delete(a, e);
                    model.drain(a as usize..e as usize);
                }
                8 => {
                    // typing run
                    let mut at = r.below(len + 1);
                    for _ in 0..10 {
                        let t = text(&mut r, 1);
                        b.insert(at, &t);
                        model.splice(at as usize..at as usize, t);
                        at += 1;
                    }
                }
                _ => {
                    // delete then undo-style reinsert of the same pieces
                    let a = r.below(len + 1);
                    let e = (a + r.below(5000)).min(len);
                    let removed = b.delete(a, e);
                    b.insert_pieces(a, &removed);
                }
            }
            if step % 300 == 0 {
                check(&b, &model);
            }
        }
        check(&b, &model);
        let snap = b.snapshot();
        let mut v = Vec::new();
        snap.read_into(0, snap.len(), &mut v);
        assert_eq!(v, model);
        b.insert(0, b"after snapshot\n");
        model.splice(0..0, b"after snapshot\n".iter().copied());
        check(&b, &model);
    }

    #[test]
    fn many_pieces_stay_balanced() {
        let mut b = Buffer::new();
        let mut model = Vec::new();
        for i in 0..20_000u64 {
            let t = format!("{i}\n");
            let at = (i * 7919) % (model.len() as u64 + 1);
            b.insert(at, t.as_bytes());
            model.splice(at as usize..at as usize, t.bytes());
        }
        assert!(b.leaves.iter().all(|l| l.pieces.len() <= LEAF_MAX));
        check(&b, &model);
        b.delete(10, model.len() as u64 - 10);
        model.drain(10..model.len() - 10);
        check(&b, &model);
    }

    #[test]
    fn line_helpers() {
        let mut b = Buffer::new();
        b.insert(0, b"one\r\ntwo\nthree");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_start(1), 5);
        assert_eq!(b.line_end_of(0), 3);
        assert_eq!(b.line_end_of(6), 8);
        assert_eq!(b.line_end_of(12), 14);
        assert_eq!(b.line_start_of(12), 9);
        assert_eq!(b.line_start(5), 14);
    }
}
