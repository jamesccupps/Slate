//! Lazy JSON structure for the path bar and the structure panel. `children` lists the direct children of one
//! container by scanning only that container's bytes; `path_at` finds the path to an offset (`data › [1203] ›
//! name`) from such lists. Nothing is parsed up front, so it works on huge files: the top level of an 800 MB
//! document is one background scan, and deeper levels are small. The scan is forgiving: broken JSON just ends the
//! list early, `//` and `/* */` comments (JSONC) are skipped, and in a file cut short whatever is still open ends at
//! the end of the file (so its structure up to there still shows).

use std::sync::Arc;

use super::buffer::Snapshot;
use super::document::Document;
use super::job::Ctx;

/// Anything that can hand out its bytes in order.
pub trait Chunked {
    fn total(&self) -> u64;
    fn chunked(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool;
    fn bytes(&self, a: u64, b: u64) -> Vec<u8> {
        let mut v = Vec::new();
        self.chunked(a, b, &mut |c| {
            v.extend_from_slice(c);
            true
        });
        v
    }
}

impl Chunked for Snapshot {
    fn total(&self) -> u64 {
        self.len()
    }
    fn chunked(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        self.chunks(a, b, f)
    }
}

impl Chunked for Document {
    fn total(&self) -> u64 {
        self.len()
    }
    fn chunked(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        self.chunks(a, b, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
    Other,
    /// An XML element with elements inside (`core::xmlnav`).
    Element,
    /// An XML element with only text (or nothing) inside.
    Leaf,
}

impl Kind {
    pub fn of(first: u8) -> Kind {
        match first {
            b'{' => Kind::Object,
            b'[' => Kind::Array,
            b'"' => Kind::String,
            b'-' | b'0'..=b'9' => Kind::Number,
            b't' | b'f' => Kind::Bool,
            b'n' => Kind::Null,
            _ => Kind::Other,
        }
    }
    pub fn is_container(self) -> bool {
        matches!(self, Kind::Object | Kind::Array | Kind::Element)
    }
}

/// One child of a container: its value's byte range, and where its key is (objects only). In an XML list
/// (`Children::xml`) a child is an element from its `<` to the end of its end tag, `key_back` is 0 and `key_len`
/// holds its start tag's length and whether it has elements inside (`xmlnav::tag_len`, `xmlnav::has_elements`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Child {
    pub start: u64,
    pub end: u64,
    /// Distance from the key's opening quote back from `start` (0 = no key).
    pub key_back: u32,
    /// Length of the key including its quotes.
    pub key_len: u32,
}

impl Child {
    pub fn key_range(&self) -> Option<(u64, u64)> {
        if self.key_len == 0 {
            None
        } else {
            let a = self.start - self.key_back as u64;
            Some((a, a + self.key_len as u64))
        }
    }
    /// Where this child begins (its key, if it has one).
    pub fn first(&self) -> u64 {
        self.start - self.key_back as u64
    }
}

/// Lists with more children than this keep only every `STRIDE`-th one (a checkpoint); the others are found by
/// rescanning at most `STRIDE` children. So a flat array of 25 million numbers costs about 10 MB, not 600 MB.
pub const DENSE_MAX: u64 = 100_000;
pub const STRIDE: u64 = 64;
/// The same for the lists of the containers a scan passes through (kept when they are big): a coordinate-heavy
/// GeoJSON is thousands of such lists, which would otherwise cost about as much memory as the file.
pub const NESTED_MAX: u64 = 1024;
/// A scan stops at a container nested deeper than this inside the one scanned (the list is partial from there, as for
/// broken JSON), and paths go no deeper. Every level costs a few hundred bytes, and a file of nothing but `[` would
/// otherwise take that for every byte (gigabytes, then an abort); this deep, at most about 25 MB. (Stopping costs the
/// scan nothing; reading on past the deep part made every scan slower.)
pub const DEPTH_MAX: usize = 50_000;

const LINE_COMMENT: u8 = 1;
const BLOCK_COMMENT: u8 = 2;

#[derive(Clone, Debug, Default)]
pub struct Children {
    /// All children, or when `stride > 1` only children 0, stride, 2·stride…
    pub items: Vec<Child>,
    /// How many children there are.
    pub count: u64,
    pub stride: u64,
    pub is_object: bool,
    /// Where the container ends (after its closing bracket), or the document end for the top level.
    pub end: u64,
    /// The scan stopped early (cancelled, or the JSON is broken).
    pub partial: bool,
    /// The children of an XML element (`core::xmlnav`), with a hash of each kept child's name in `names`, and
    /// whether all its children have the same name.
    pub xml: bool,
    pub names: Vec<u32>,
    pub same_names: bool,
    /// XML: where the element's content ends (its end tag's `<`, or where an end tag of one around it closed it), so
    /// a scan again between its children stops there as the first one did.
    pub content_end: u64,
}

impl Children {
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Up to `count` children from `from` (where a child begins), scanned again.
    fn rescan(&self, src: &dyn Chunked, from: u64, count: u64) -> Vec<Child> {
        if self.xml {
            super::xmlnav::rescan(src, from, count, self.content_end)
        } else {
            rescan(src, from, self.is_object, count)
        }
    }

    /// Children `a..b`.
    pub fn range(&self, src: &dyn Chunked, a: u64, b: u64) -> Vec<Child> {
        let b = b.min(self.count);
        if a >= b {
            return Vec::new();
        }
        if self.stride <= 1 {
            return self.items[a as usize..(b as usize).min(self.items.len())].to_vec();
        }
        let k = a / self.stride;
        let Some(cp) = self.items.get(k as usize) else { return Vec::new() };
        let base = k * self.stride;
        self.rescan(src, cp.first(), b - base).into_iter().skip((a - base) as usize).collect()
    }

    pub fn get(&self, src: &dyn Chunked, i: u64) -> Option<Child> {
        self.range(src, i, i + 1).pop()
    }

    /// The child that `offset` is in (its key or its value), with its index.
    pub fn find(&self, src: &dyn Chunked, offset: u64) -> Option<(u64, Child)> {
        let k = self.items.partition_point(|c| c.first() <= offset).checked_sub(1)?;
        if self.stride <= 1 {
            let c = self.items[k];
            return (offset <= c.end).then_some((k as u64, c));
        }
        let base = k as u64 * self.stride;
        let n = self.stride.min(self.count - base);
        let block = self.rescan(src, self.items[k].first(), n);
        let j = block.partition_point(|c| c.first() <= offset).checked_sub(1)?;
        let c = block[j];
        (offset <= c.end).then_some((base + j as u64, c))
    }

    /// The child whose value starts at `start`.
    pub fn find_start(&self, src: &dyn Chunked, start: u64) -> Option<Child> {
        self.find(src, start).map(|(_, c)| c).filter(|c| c.start == start)
    }
}

/// Up to `count` children of a container, starting at `from` (where a child begins).
fn rescan(src: &dyn Chunked, from: u64, is_object: bool, count: u64) -> Vec<Child> {
    scan_core(src, None, from, is_object, false, u64::MAX, None, count, None).pop().map(|(_, c)| c.items).unwrap_or_default()
}

const C_OTHER: u8 = 0;
const C_WS: u8 = 1;
const C_QUOTE: u8 = 2;
const C_OPEN: u8 = 3;
const C_CLOSE: u8 = 4;
const C_COMMA: u8 = 5;
const C_COLON: u8 = 6;
/// The `/` of `//` or `/*` (worked out when it's seen; CLASS has it as C_OTHER).
const C_COMMENT: u8 = 7;

const fn classes() -> [u8; 256] {
    let mut t = [C_OTHER; 256];
    t[b' ' as usize] = C_WS;
    t[b'\t' as usize] = C_WS;
    t[b'\r' as usize] = C_WS;
    t[b'\n' as usize] = C_WS;
    t[b'"' as usize] = C_QUOTE;
    t[b'{' as usize] = C_OPEN;
    t[b'[' as usize] = C_OPEN;
    t[b'}' as usize] = C_CLOSE;
    t[b']' as usize] = C_CLOSE;
    t[b',' as usize] = C_COMMA;
    t[b':' as usize] = C_COLON;
    t
}
static CLASS: [u8; 256] = classes();

/// Lists the direct children of the container whose opening bracket is at `open`; with `open == None`, the
/// top-level values of the document (one for normal JSON, one per record for JSON Lines).
pub fn children(src: &dyn Chunked, open: Option<u64>, ctx: Option<&Ctx>) -> Children {
    scan(src, open, u64::MAX, None, ctx).pop().map(|(_, c)| c).unwrap_or_default()
}

/// One level of the scanner's stack: a container being read.
struct Level {
    open: Option<u64>,
    is_object: bool,
    expect_key: bool,
    key: Option<(u64, u64)>,
    val_start: Option<u64>,
    scalar_last: Option<u64>,
    /// Where this level's children start in the shared list.
    items_from: usize,
    count: u64,
    /// Only every STRIDE-th child is kept (the level has more than DENSE_MAX, or NESTED_MAX when `nested`).
    sparse: bool,
    /// Never thin this level (a rescan, which must return every child it is asked for).
    dense: bool,
    /// A container inside the scanned one (its list is kept only if it is big).
    nested: bool,
}

impl Level {
    fn new(open: Option<u64>, is_object: bool, items_from: usize, nested: bool) -> Level {
        Level {
            open,
            is_object,
            expect_key: is_object,
            key: None,
            val_start: None,
            scalar_last: None,
            items_from,
            count: 0,
            sparse: false,
            dense: false,
            nested,
        }
    }

    fn list(&self, items: Vec<Child>, end: u64, partial: bool) -> Children {
        Children {
            items,
            count: self.count,
            stride: if self.sparse { STRIDE } else { 1 },
            is_object: self.is_object,
            end,
            partial,
            ..Default::default()
        }
    }
}

/// Adds a child to `lv` (whose children are at the end of `items`), thinning the list once it gets big.
fn add_child(items: &mut Vec<Child>, lv: &mut Level, s: u64, e: u64) {
    let (key_back, key_len) = match lv.key.take() {
        Some((ks, ke)) if ks < s => ((s - ks).min(u32::MAX as u64) as u32, (ke - ks).min(u32::MAX as u64) as u32),
        _ => (0, 0),
    };
    if !lv.sparse && !lv.dense && lv.count >= if lv.nested { NESTED_MAX } else { DENSE_MAX } {
        let from = lv.items_from;
        let mut w = from;
        for k in (from..items.len()).step_by(STRIDE as usize) {
            items[w] = items[k];
            w += 1;
        }
        items.truncate(w);
        lv.sparse = true;
    }
    if !lv.sparse || lv.count % STRIDE == 0 {
        items.push(Child { start: s, end: e, key_back, key_len });
    }
    lv.count += 1;
}

/// Scans the container at `open` (None = the whole document) in one pass. Returns its child list last, preceded by
/// the lists of all containers inside it that are at least `keep` bytes long, so a single read of a huge file
/// covers every big level, or that hold `path_to` (all of the path to it, from one scan).
pub fn scan(
    src: &dyn Chunked,
    open: Option<u64>,
    keep: u64,
    path_to: Option<u64>,
    ctx: Option<&Ctx>,
) -> Vec<(Option<u64>, Children)> {
    let (start, root_obj) = match open {
        Some(o) => (o + 1, src.bytes(o, o + 1).first() == Some(&b'{')),
        None => (0, false),
    };
    scan_core(src, open, start, root_obj, open.is_none(), keep, path_to, u64::MAX, ctx)
}

/// The scanner. Reads from `start` (inside the container at `open`, or at the top level when `top`), stops at the
/// container's end or after `limit` children. `//` and `/* */` comments (JSONC) count as whitespace.
#[allow(clippy::too_many_arguments)]
fn scan_core(
    src: &dyn Chunked,
    open: Option<u64>,
    start: u64,
    root_obj: bool,
    top: bool,
    keep: u64,
    path_to: Option<u64>,
    limit: u64,
    ctx: Option<&Ctx>,
) -> Vec<(Option<u64>, Children)> {
    let total = src.total();
    // a container's list is kept when it's big, or holds `path_to`
    let kept = |cs: u64, end: u64| end - cs >= keep || path_to.is_some_and(|q| cs < q && q < end);
    let mut items: Vec<Child> = Vec::new();
    let mut stack: Vec<Level> = vec![Level::new(open, root_obj, 0, false)];
    stack[0].dense = limit != u64::MAX;
    let mut out: Vec<(Option<u64>, Children)> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    let mut str_start = 0u64;
    // In a comment (and a `*` of a block comment ended the last chunk); a `/` ended the last chunk (at `slash_at`).
    let mut comment = 0u8;
    let mut star = false;
    let mut slash = false;
    let mut slash_at = 0u64;
    let mut end: Option<u64> = None;
    let mut broken = false;
    let mut pos = start;
    let mut since_check = 0u64;

    src.chunked(start, total, &mut |chunk: &[u8]| {
        let base = pos;
        let n = chunk.len();
        let mut i = 0usize;
        while i < n {
            if in_str {
                if esc {
                    esc = false;
                    i += 1;
                    continue;
                }
                match memchr::memchr2(b'"', b'\\', &chunk[i..]) {
                    None => i = n,
                    Some(k) => {
                        i += k;
                        if chunk[i] == b'\\' {
                            esc = true;
                        } else {
                            in_str = false;
                            let p = base + i as u64;
                            let lv = stack.last_mut().unwrap();
                            if lv.is_object && lv.expect_key {
                                lv.key = Some((str_start, p + 1));
                            } else {
                                add_child(&mut items, lv, str_start, p + 1);
                                lv.val_start = None;
                                if stack.len() == 1 && stack[0].count >= limit {
                                    end = Some(p + 1);
                                    return false;
                                }
                            }
                        }
                        i += 1;
                    }
                }
                continue;
            }
            if comment == LINE_COMMENT {
                match memchr::memchr(b'\n', &chunk[i..]) {
                    None => i = n,
                    Some(k) => {
                        i += k + 1;
                        comment = 0;
                    }
                }
                continue;
            }
            if comment == BLOCK_COMMENT {
                if star && chunk[i] == b'/' {
                    comment = 0;
                    i += 1;
                }
                star = false;
                if comment == 0 {
                    continue;
                }
                match memchr::memchr(b'*', &chunk[i..]) {
                    None => i = n,
                    Some(k) => {
                        i += k + 1;
                        if i == n {
                            star = true;
                        } else if chunk[i] == b'/' {
                            comment = 0;
                            i += 1;
                        }
                    }
                }
                continue;
            }
            let b = chunk[i];
            let p = base + i as u64;
            if slash {
                slash = false;
                let lv = stack.last_mut().unwrap();
                if b == b'/' || b == b'*' {
                    // The last chunk ended with the `/` of a comment, which ends a scalar like whitespace.
                    if let Some(last) = lv.scalar_last.take() {
                        let s = lv.val_start.take().unwrap_or(last);
                        add_child(&mut items, lv, s, last + 1);
                        if stack.len() == 1 && stack[0].count >= limit {
                            end = Some(slash_at);
                            return false;
                        }
                    }
                    comment = if b == b'/' { LINE_COMMENT } else { BLOCK_COMMENT };
                    i += 1;
                    continue;
                }
                // a stray `/`, part of a scalar
                if lv.scalar_last.is_none() {
                    lv.val_start = Some(slash_at);
                }
                lv.scalar_last = Some(slash_at);
            }
            let mut c = CLASS[b as usize];
            if b == b'/' {
                match chunk.get(i + 1) {
                    Some(b'/' | b'*') => c = C_COMMENT,
                    Some(_) => {}
                    None => {
                        slash = true;
                        slash_at = p;
                        i += 1;
                        continue;
                    }
                }
            }
            let lv = stack.last_mut().unwrap();
            if let Some(last) = lv.scalar_last {
                if c == C_OTHER {
                    // the rest of the scalar at once (a `/` may start a comment: that's looked at on its own)
                    let k = chunk[i + 1..].iter().position(|&x| CLASS[x as usize] != C_OTHER || x == b'/');
                    i = k.map_or(n, |k| i + 1 + k);
                    lv.scalar_last = Some(base + i as u64 - 1);
                    continue;
                }
                let s = lv.val_start.take().unwrap_or(last);
                add_child(&mut items, lv, s, last + 1);
                lv.scalar_last = None;
                if stack.len() == 1 && stack[0].count >= limit {
                    end = Some(p);
                    return false;
                }
            }
            let lv = stack.last_mut().unwrap();
            match c {
                C_WS => {}
                C_COMMENT => {
                    comment = if chunk[i + 1] == b'/' { LINE_COMMENT } else { BLOCK_COMMENT };
                    i += 2;
                    continue;
                }
                C_QUOTE => {
                    in_str = true;
                    str_start = p;
                    if !(lv.is_object && lv.expect_key) {
                        lv.val_start = Some(p);
                    }
                }
                C_OPEN => {
                    lv.val_start = Some(p);
                    if stack.len() > DEPTH_MAX {
                        broken = true;
                        end = Some(p);
                        return false;
                    }
                    let from = items.len();
                    stack.push(Level::new(Some(p), b == b'{', from, true));
                }
                C_CLOSE => {
                    if stack.len() == 1 {
                        // The scanned container ends (or a stray closer at the top level: broken JSON).
                        broken |= top;
                        end = Some(p + 1);
                        return false;
                    }
                    let child = stack.pop().unwrap();
                    let cs = child.open.unwrap();
                    if kept(cs, p + 1) {
                        let list = items.split_off(child.items_from);
                        out.push((child.open, child.list(list, p + 1, false)));
                    } else {
                        items.truncate(child.items_from);
                    }
                    let parent = stack.last_mut().unwrap();
                    let s = parent.val_start.take().unwrap_or(cs);
                    add_child(&mut items, parent, s, p + 1);
                    if stack.len() == 1 && stack[0].count >= limit {
                        end = Some(p + 1);
                        return false;
                    }
                }
                C_COMMA => lv.expect_key = lv.is_object,
                C_COLON => lv.expect_key = false,
                _ => {
                    lv.val_start = Some(p);
                    lv.scalar_last = Some(p);
                }
            }
            i += 1;
        }
        pos = base + n as u64;
        since_check += n as u64;
        if since_check >= 4 << 20 {
            since_check = 0;
            if let Some(c) = ctx {
                c.set(pos);
                if c.cancelled() {
                    return false;
                }
            }
        }
        true
    });

    let cancelled = ctx.is_some_and(|c| c.cancelled());
    let mut partial = broken || cancelled;
    if end.is_none() && !cancelled {
        // Ran to the end of the document without the container closing (a truncated file, say): what is still
        // open ends there, so everything up to that point can be shown.
        let unclosed = stack.len() > 1;
        let lv = stack.last_mut().unwrap();
        if slash {
            if lv.scalar_last.is_none() {
                lv.val_start = Some(slash_at);
            }
            lv.scalar_last = Some(slash_at);
        }
        if in_str {
            if !(lv.is_object && lv.expect_key) {
                add_child(&mut items, lv, str_start, total);
                lv.val_start = None;
            }
        } else if let Some(last) = lv.scalar_last.take() {
            let s = lv.val_start.take().unwrap_or(last);
            add_child(&mut items, lv, s, last + 1);
        }
        while stack.len() > 1 {
            let child = stack.pop().unwrap();
            let cs = child.open.unwrap();
            if kept(cs, total) {
                let list = items.split_off(child.items_from);
                out.push((child.open, child.list(list, total, true)));
            } else {
                items.truncate(child.items_from);
            }
            let parent = stack.last_mut().unwrap();
            let s = parent.val_start.take().unwrap_or(cs);
            add_child(&mut items, parent, s, total);
        }
        partial |= !top || unclosed || in_str;
    }
    let own = stack.get(1).map_or(items.len(), |l| l.items_from);
    items.truncate(own);
    let root = &stack[0];
    out.push((open, root.list(items, end.unwrap_or(total), partial)));
    out
}

/// The first version of the scanner, kept for comparison in tests.
#[cfg(test)]
fn children_simple(src: &dyn Chunked, open: Option<u64>, limit: u64, ctx: Option<&Ctx>) -> Children {
    let total = src.total().min(limit);
    let (start, is_object) = match open {
        Some(o) => {
            let b = src.bytes(o, o + 1);
            (o + 1, b.first() == Some(&b'{'))
        }
        None => (0, false),
    };
    let top = open.is_none();
    let mut out = Children { items: Vec::new(), count: 0, stride: 1, is_object, end: total, partial: false, ..Default::default() };
    let mut depth: u32 = 0;
    let mut in_str = false;
    let mut esc = false;
    let mut str_start = 0u64;
    let mut expect_key = is_object;
    let mut key: Option<(u64, u64)> = None;
    let mut val_start: Option<u64> = None;
    // A number / true / false / null in progress at depth 0, and its last byte.
    let mut scalar_last: Option<u64> = None;
    let mut done = false;
    let mut pos = start;
    let mut since_check = 0u64;

    let push = |out: &mut Children, key: &mut Option<(u64, u64)>, s: u64, e: u64| {
        let (key_back, key_len) = match key.take() {
            Some((ks, ke)) if ks < s => ((s - ks).min(u32::MAX as u64) as u32, (ke - ks).min(u32::MAX as u64) as u32),
            _ => (0, 0),
        };
        out.items.push(Child { start: s, end: e, key_back, key_len });
        out.count += 1;
    };

    src.chunked(start, total, &mut |chunk: &[u8]| {
        let base = pos;
        let mut i = 0usize;
        let n = chunk.len();
        while i < n {
            if in_str {
                // Jump to the next quote or backslash.
                if esc {
                    esc = false;
                    i += 1;
                    continue;
                }
                match memchr::memchr2(b'"', b'\\', &chunk[i..]) {
                    None => {
                        i = n;
                        continue;
                    }
                    Some(k) => {
                        i += k;
                        if chunk[i] == b'\\' {
                            esc = true;
                            i += 1;
                            continue;
                        }
                        in_str = false;
                        let p = base + i as u64;
                        if depth == 0 {
                            if expect_key {
                                key = Some((str_start, p + 1));
                            } else {
                                push(&mut out, &mut key, str_start, p + 1);
                                val_start = None;
                            }
                        }
                        i += 1;
                        continue;
                    }
                }
            }
            if depth > 0 {
                // Inside a nested container: only quotes and brackets matter.
                while i < n {
                    let c = CLASS[chunk[i] as usize];
                    if c == C_QUOTE || c == C_OPEN || c == C_CLOSE {
                        break;
                    }
                    i += 1;
                }
                if i >= n {
                    break;
                }
                let p = base + i as u64;
                match CLASS[chunk[i] as usize] {
                    C_QUOTE => in_str = true,
                    C_OPEN => depth += 1,
                    _ => {
                        depth -= 1;
                        if depth == 0 {
                            if let Some(s) = val_start.take() {
                                push(&mut out, &mut key, s, p + 1);
                            }
                        }
                    }
                }
                i += 1;
                continue;
            }
            // Depth 0: directly inside the container (or at the top level).
            let b = chunk[i];
            let p = base + i as u64;
            let c = CLASS[b as usize];
            if let Some(last) = scalar_last {
                if c == C_OTHER {
                    scalar_last = Some(p);
                    i += 1;
                    continue;
                }
                push(&mut out, &mut key, val_start.take().unwrap_or(last), last + 1);
                scalar_last = None;
            }
            match c {
                C_WS => {}
                C_QUOTE => {
                    in_str = true;
                    str_start = p;
                    if !expect_key {
                        val_start = Some(p);
                    }
                }
                C_OPEN => {
                    val_start = Some(p);
                    depth = 1;
                }
                C_CLOSE => {
                    if top {
                        // Stray closer at the top level: broken JSON.
                        out.partial = true;
                    }
                    out.end = p + 1;
                    done = true;
                    return false;
                }
                C_COMMA => expect_key = is_object,
                C_COLON => expect_key = false,
                _ => {
                    val_start = Some(p);
                    scalar_last = Some(p);
                }
            }
            i += 1;
        }
        pos = base + n as u64;
        since_check += n as u64;
        if since_check >= 4 << 20 {
            since_check = 0;
            if let Some(c) = ctx {
                c.set(pos);
                if c.cancelled() {
                    return false;
                }
            }
        }
        true
    });
    if !done {
        if let Some(last) = scalar_last {
            push(&mut out, &mut key, val_start.unwrap_or(last), last + 1);
        }
        if !top || depth > 0 || in_str {
            out.partial = true;
        }
        if ctx.is_some_and(|c| c.cancelled()) {
            out.partial = true;
        }
    }
    out
}

/// One step of a path: the child's index and its key (if in an object; for XML the element's name), with its value
/// range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub index: u64,
    pub key: Option<String>,
    pub start: u64,
    pub end: u64,
    /// The container this step is a child of (None = top level).
    pub parent: Option<u64>,
    /// XML: which of the elements of its name it is among its siblings, from 1 (`book[3]`); 0 when no sibling has
    /// that name, `ORD_UNKNOWN` in a huge list of mixed names. Always 0 for JSON.
    pub ord: u64,
}

pub const ORD_UNKNOWN: u64 = u64::MAX;

impl Step {
    /// The step as the path bar shows it.
    pub fn label(&self) -> String {
        match &self.key {
            Some(k) if self.ord > 0 && self.ord != ORD_UNKNOWN => format!("{}[{}]", key_label(k), self.ord),
            Some(k) => key_label(k),
            None => format!("[{}]", self.index),
        }
    }
}

/// A key's exact text: quotes removed and every escape resolved (`\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`,
/// `\uXXXX` with surrogate pairs).
pub fn key_text(raw: &[u8]) -> String {
    let inner = raw.strip_prefix(b"\"").unwrap_or(raw);
    let inner = inner.strip_suffix(b"\"").unwrap_or(inner);
    if !inner.contains(&b'\\') {
        return String::from_utf8_lossy(inner).into_owned();
    }
    let hex = |s: Option<&[u8]>| s.and_then(|s| std::str::from_utf8(s).ok()).and_then(|h| u32::from_str_radix(h, 16).ok());
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' || i + 1 == inner.len() {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        let e = inner[i + 1];
        i += 2;
        match e {
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                let Some(mut cp) = hex(inner.get(i..i + 4)) else {
                    out.extend_from_slice(b"\\u");
                    continue;
                };
                i += 4;
                if (0xD800..0xDC00).contains(&cp) && inner.get(i..i + 2) == Some(b"\\u") {
                    if let Some(lo) = hex(inner.get(i + 2..i + 6)).filter(|lo| (0xDC00..0xE000).contains(lo)) {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        i += 6;
                    }
                }
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            // `\"`, `\\`, `\/` (and anything else, as it is)
            c => out.push(c),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A key as the path bar and the panel show it: line breaks and other control characters as symbols, very long
/// keys shortened.
pub fn key_label(key: &str) -> String {
    let mut s = String::new();
    for (n, c) in key.chars().enumerate() {
        if n == 200 {
            s.push('…');
            break;
        }
        s.push(match c {
            '\n' => '⏎',
            '\t' => ' ',
            c if c.is_control() => '·',
            c => c,
        });
    }
    s
}

/// What a path walk needs before it can go on: the container to scan (None = the top level) and where it ends.
pub type Missing = (Option<u64>, u64);

/// The step of an earlier walk (`prev`, over the same text) that the walk meets again at depth `k`: the same child
/// of the same container, which needn't be read again.
pub(crate) fn same_step(prev: &[Step], k: usize, open: Option<u64>, i: u64, start: u64) -> Option<&Step> {
    prev.get(k).filter(|s| s.parent == open && s.index == i && s.start == start)
}

/// The path to `offset`. `get(open)` returns the cached children of a container (None = top level); when one
/// isn't known yet this returns `Err` with it (and where it ends) so the caller can scan it and ask again. `prev` is
/// the path worked out last for the same text: where this one goes the same way, its steps are taken as they are.
pub fn path_at(
    src: &dyn Chunked,
    offset: u64,
    prev: &[Step],
    get: &mut dyn FnMut(Option<u64>) -> Option<Arc<Children>>,
) -> Result<Vec<Step>, Missing> {
    let mut path: Vec<Step> = Vec::new();
    let (mut open, mut end): (Option<u64>, u64) = (None, src.total());
    loop {
        let Some(ch) = get(open) else { return Err((open, end)) };
        let Some((i, c)) = ch.find(src, offset) else { break };
        // A single top-level value (normal JSON, also when the file is cut short) isn't shown as "[0]".
        let single_root = open.is_none() && ch.count == 1;
        let k = path.len();
        let again = same_step(prev, k, open, i, c.start).filter(|_| !single_root);
        if !single_root {
            let key = match again {
                Some(s) => s.key.clone(),
                None => c.key_range().map(|(a, b)| key_text(&src.bytes(a, b))),
            };
            path.push(Step { index: i, key, start: c.start, end: c.end, parent: open, ord: 0 });
        }
        // (a step the last walk went into is a container)
        let container = (again.is_some() && prev.get(k + 1).is_some_and(|n| n.parent == Some(c.start)))
            || src.bytes(c.start, c.start + 1).first().is_some_and(|&b| Kind::of(b).is_container());
        if container && offset > c.start && offset < c.end && path.len() < DEPTH_MAX {
            (open, end) = (Some(c.start), c.end);
            continue;
        }
        break;
    }
    Ok(path)
}

/// A short preview of a value for the structure panel.
pub fn preview(src: &dyn Chunked, c: &Child, max: usize) -> (Kind, String) {
    let head = src.bytes(c.start, (c.start + max as u64 * 4).min(c.end));
    let kind = head.first().map_or(Kind::Other, |&b| Kind::of(b));
    let text = match kind {
        Kind::Object | Kind::Array => String::new(),
        _ => {
            let s = String::from_utf8_lossy(&head).into_owned();
            let mut t: String = s.chars().take(max).collect();
            if (c.end - c.start) as usize > t.len() {
                t.push('…');
            }
            t
        }
    };
    (kind, text)
}

/// Path text for copying, as JavaScript writes it: `data[1203].name`, `["my key"]`, `["1234"]`.
pub fn path_string(path: &[Step]) -> String {
    let ident = |k: &str| {
        let mut cs = k.chars();
        cs.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
            && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    };
    let mut s = String::new();
    for st in path {
        match &st.key {
            Some(k) if ident(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            // a JSON string literal is a valid JavaScript one
            Some(k) => s.push_str(&format!("[{}]", serde_json::to_string(k).unwrap_or_default())),
            None => s.push_str(&format!("[{}]", st.index)),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use std::collections::HashMap;

    fn doc(s: &str) -> Document {
        Document::from_text(s.as_bytes())
    }

    fn all(d: &Document, ch: &Children) -> Vec<Child> {
        ch.range(d, 0, ch.count)
    }

    fn texts(d: &Document, ch: &Children) -> Vec<(Option<String>, String)> {
        all(d, ch)
            .iter()
            .map(|c| {
                let k = c.key_range().map(|(a, b)| key_text(&d.read(a, b)));
                (k, String::from_utf8(d.read(c.start, c.end)).unwrap())
            })
            .collect()
    }

    #[test]
    fn lists_children() {
        let s = r#"{"a": 1, "b" : [1, {"x": "]}"}, 3], "c":{"d":null} , "e": "s\"t", "f": -2.5e3 }"#;
        let d = doc(s);
        let top = children(&d, None, None);
        assert_eq!(top.count, 1);
        assert!(!top.partial);
        let root = children(&d, Some(0), None);
        let t = texts(&d, &root);
        assert_eq!(
            t,
            vec![
                (Some("a".into()), "1".into()),
                (Some("b".into()), r#"[1, {"x": "]}"}, 3]"#.into()),
                (Some("c".into()), r#"{"d":null}"#.into()),
                (Some("e".into()), r#""s\"t""#.into()),
                (Some("f".into()), "-2.5e3".into()),
            ]
        );
        assert_eq!(root.end, s.len() as u64);
        let b = root.get(&d, 1).unwrap();
        let arr = children(&d, Some(b.start), None);
        assert_eq!(texts(&d, &arr).len(), 3);
        assert_eq!(texts(&d, &arr)[1].1, r#"{"x": "]}"}"#);
    }

    #[test]
    fn json_lines_top_level() {
        let d = doc("{\"a\":1}\n{\"a\":2}\n[3]\n\"x\"\n42\n");
        let top = children(&d, None, None);
        let t: Vec<String> = texts(&d, &top).into_iter().map(|x| x.1).collect();
        assert_eq!(t, vec!["{\"a\":1}", "{\"a\":2}", "[3]", "\"x\"", "42"]);
    }

    #[test]
    fn paths() {
        let s = r#"{"data": [{"id": 1, "name": "x"}, {"id": 2, "name": "yy", "tags": ["a", "b"]}], "n": 5}"#;
        let d = doc(s);
        let mut cache: HashMap<Option<u64>, Arc<Children>> = HashMap::new();
        let mut path = |off: u64| loop {
            let r = path_at(&d, off, &[], &mut |o| cache.get(&o).cloned());
            match r {
                Ok(p) => return path_string(&p),
                Err((o, _)) => {
                    let ch = children(&d, o, None);
                    cache.insert(o, Arc::new(ch));
                }
            }
        };
        let at = |needle: &str| s.find(needle).unwrap() as u64;
        assert_eq!(path(at("\"yy\"") + 1), "data[1].name");
        assert_eq!(path(at("\"b\"]")), "data[1].tags[1]");
        assert_eq!(path(at("\"id\": 2")), "data[1].id");
        assert_eq!(path(at("5}")), "n");
        assert_eq!(path(0), "");
    }

    #[test]
    fn broken_json_is_partial() {
        let d = doc(r#"{"a": [1, 2"#);
        let root = children(&d, Some(0), None);
        assert!(root.partial);
        // the open array ends where the file does
        assert_eq!(texts(&d, &root), vec![(Some("a".into()), "[1, 2".into())]);
        let arr = children(&d, Some(6), None);
        assert!(arr.partial);
        assert_eq!(texts(&d, &arr).into_iter().map(|x| x.1).collect::<Vec<_>>(), vec!["1", "2"]);
    }

    /// Paths at every offset, scanning what's needed.
    fn all_paths(src: &dyn Chunked) -> Vec<String> {
        let mut cache: HashMap<Option<u64>, Arc<Children>> = HashMap::new();
        (0..=src.total())
            .map(|off| loop {
                match path_at(src, off, &[], &mut |o| cache.get(&o).cloned()) {
                    Ok(p) => break path_string(&p),
                    Err((o, _)) => {
                        let ch = children(src, o, None);
                        cache.insert(o, Arc::new(ch));
                    }
                }
            })
            .collect()
    }

    #[test]
    fn a_truncated_file_keeps_its_structure() {
        let s = r#"{"meta": {"n": 3}, "data": [{"id": 1}, {"id": 2}, {"id": 3, "name": "thr"#;
        let d = doc(s);
        let top = children(&d, None, None);
        assert_eq!((top.count, top.partial), (1, true));
        let root = children(&d, Some(0), None);
        assert_eq!(texts(&d, &root).iter().map(|t| t.0.clone().unwrap()).collect::<Vec<_>>(), ["meta", "data"]);
        assert_eq!(root.get(&d, 1).unwrap().end, s.len() as u64);
        let paths = all_paths(&d);
        assert_eq!(paths[s.find("thr").unwrap()], "data[2].name");
        assert_eq!(paths[s.find("3,").unwrap()], "data[2].id");
        // the background scan keeps the open containers' lists too
        let lists: HashMap<Option<u64>, Children> = scan(&d, None, 1, None, None).into_iter().collect();
        let data = &lists[&Some(s.find('[').unwrap() as u64)];
        assert_eq!((data.count, data.partial, data.end), (3, true, s.len() as u64));
    }

    /// Hands out its bytes in pieces of `.1` bytes.
    struct Pieces<'a>(&'a [u8], usize);
    impl Chunked for Pieces<'_> {
        fn total(&self) -> u64 {
            self.0.len() as u64
        }
        fn chunked(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
            self.0[a as usize..b as usize].chunks(self.1).all(|c| f(c))
        }
    }

    #[test]
    fn comments_count_as_whitespace() {
        let s = "// head: [x\n{\"a\": 1 /* b: [2, {}] */, \"c\" /**/: [1, 2] // done }\n, \"d\": \"no // comment\", \"e\": 5/**/ }\n";
        // the same text with the comments blanked out must have the same structure
        let mut blank = s.as_bytes().to_vec();
        for (a, b) in [("//", "\n"), ("/*", "*/")] {
            let mut from = 0;
            while let Some(p) = s[from..].find(a).map(|p| p + from) {
                if s[..p].matches('"').count() % 2 == 1 {
                    from = p + 2;
                    continue;
                }
                let e = s[p + 2..].find(b).map_or(s.len(), |q| p + 2 + q + if b == "*/" { 2 } else { 0 });
                blank[p..e].iter_mut().for_each(|c| *c = b' ');
                from = e;
            }
        }
        let want = all_paths(&Pieces(&blank, 1000));
        assert!(want.contains(&"c[1]".to_string()) && want.contains(&"e".to_string()));
        for step in [1, 2, 3, 7, 1000] {
            let src = Pieces(s.as_bytes(), step);
            assert_eq!(all_paths(&src), want, "pieces of {step}");
            let top = children(&src, None, None);
            assert_eq!((top.count, top.partial), (1, false));
            assert_eq!(children(&src, Some(12), None).count, 4, "pieces of {step}");
        }
    }

    #[test]
    fn keys_are_decoded_and_copied_as_valid_paths() {
        assert_eq!(key_text(br#""a\\nb""#), "a\\nb");
        assert_eq!(key_text(br#""a\nb\t\"q\"\/""#), "a\nb\t\"q\"/");
        // (the escapes are put together so they stay escapes in this file)
        let raw = format!("\"caf{0}u00e9 {0}ud83d{0}ude00 {0}u00\"", '\\');
        assert_eq!(key_text(raw.as_bytes()), "café 😀 \\u00");
        assert_eq!(key_label("a\nb\tc\u{1}"), "a⏎b c·");
        assert_eq!(key_label(&"k".repeat(300)).chars().count(), 201);
        let s = r#"{"1234": {"x y": [10, 20]}, "é": {"a\nb": 1, "$ok_1": 2, "say \"hi\"": 3, "": 4}}"#;
        let d = doc(s);
        let paths = all_paths(&d);
        assert_eq!(paths[s.find("20").unwrap()], r#"["1234"]["x y"][1]"#);
        assert_eq!(paths[s.find("1,").unwrap()], r#"é["a\nb"]"#);
        assert_eq!(paths[s.find("2,").unwrap()], "é.$ok_1");
        assert_eq!(paths[s.find("3,").unwrap()], r#"é["say \"hi\""]"#);
        assert_eq!(paths[s.find("4}").unwrap()], r#"é[""]"#);
    }

    #[test]
    fn nested_lists_are_thinned_sooner() {
        // 300 rings of 3000 points, like a GeoJSON file
        let ring = format!("[{}]", vec!["[12.3456,45.6789]"; 3000].join(","));
        let s = format!("{{\"features\": [{}]}}", vec![ring.as_str(); 300].join(","));
        let d = doc(&s);
        let lists = scan(&d, None, 4096, None, None);
        let items: usize = lists.iter().map(|(_, c)| c.items.len()).sum();
        let children_total: u64 = lists.iter().map(|(_, c)| c.count).sum();
        assert!(children_total > 900_000 && items < 20_000, "{items} kept of {children_total}");
        let (open, ring_list) = lists.iter().find(|(_, c)| c.count == 3000).unwrap();
        assert_eq!(ring_list.stride, STRIDE);
        let simple = children_simple(&d, *open, u64::MAX, None);
        assert_eq!(all(&d, ring_list), simple.items);
        for i in [0, 63, 64, 1000, 2999] {
            let c = simple.items[i as usize];
            assert_eq!(ring_list.find(&d, c.start), Some((i, c)));
        }
    }

    fn rnd(r: &mut u64) -> u64 {
        *r ^= *r << 13;
        *r ^= *r >> 7;
        *r ^= *r << 17;
        *r
    }

    fn random_json(r: &mut u64, depth: u32, out: &mut String) {
        let ws = |n: u64, out: &mut String| {
            for _ in 0..(n % 3) {
                out.push(if n % 2 == 0 { ' ' } else { '\n' });
            }
        };
        let k = rnd(r) % if depth > 4 { 4 } else { 7 };
        match k {
            0 => out.push_str(&format!("{}", rnd(r) % 1000)),
            1 => out.push_str("\"s\\\"t]}{[,:\""),
            2 => out.push_str(["true", "false", "null", "-1.5e3"][(rnd(r) % 4) as usize]),
            3 => out.push_str("\"plain\""),
            4 | 5 => {
                out.push('[');
                let n = rnd(r) % 6;
                for i in 0..n {
                    if i > 0 {
                        out.push(',');
                    }
                    let w = rnd(r);
                    ws(w, out);
                    random_json(r, depth + 1, out);
                }
                out.push(']');
            }
            _ => {
                out.push('{');
                let n = rnd(r) % 6;
                for i in 0..n {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("\"k{i}\\\"\" :"));
                    let w = rnd(r);
                    ws(w, out);
                    random_json(r, depth + 1, out);
                }
                out.push('}');
            }
        }
    }

    /// Opening brackets outside strings.
    fn containers(s: &[u8]) -> Vec<u64> {
        let (mut in_str, mut esc) = (false, false);
        let mut v = Vec::new();
        for (i, &b) in s.iter().enumerate() {
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if b == b'"' {
                in_str = true;
            } else if b == b'{' || b == b'[' {
                v.push(i as u64);
            }
        }
        v
    }

    #[test]
    fn one_pass_scan_matches_the_simple_scanner() {
        let mut r = 0x1234_5678_9ABC_DEF0u64;
        for round in 0..200 {
            let mut s = String::new();
            let records = 1 + round % 4;
            for i in 0..records {
                if i > 0 {
                    s.push('\n');
                }
                random_json(&mut r, 0, &mut s);
            }
            let d = doc(&s);
            let top_new = children(&d, None, None);
            let top_old = children_simple(&d, None, u64::MAX, None);
            assert_eq!(all(&d, &top_new), top_old.items, "top level of {s}");
            let lists: HashMap<Option<u64>, Children> = scan(&d, None, 20, None, None).into_iter().collect();
            for o in containers(s.as_bytes()) {
                let old = children_simple(&d, Some(o), u64::MAX, None);
                let new = children(&d, Some(o), None);
                assert_eq!(all(&d, &new), old.items, "container at {o} of {s}");
                assert_eq!(new.end, old.end);
                if let Some(kept) = lists.get(&Some(o)) {
                    assert_eq!(all(&d, kept), old.items, "kept list at {o}");
                    assert!(kept.end - o >= 20);
                } else {
                    assert!(old.end - o < 20 || old.partial, "big container at {o} not kept: {s}");
                }
            }
        }
    }

    #[test]
    fn nesting_too_deep_to_follow_stops_the_scan_and_costs_little() {
        // a file of nothing but `[`, and one that closes with something after it
        let deep = DEPTH_MAX + 1000;
        for s in ["[".repeat(deep * 3), format!("[{}1{}, \"after\"]", "[".repeat(deep), "]".repeat(deep))] {
            let d = doc(&s);
            let lists = scan(&d, None, u64::MAX, Some(d.len() / 2), None);
            // (the lists of the containers followed, and no more; partial from where it stopped)
            assert!(lists.len() <= DEPTH_MAX + 2, "{} lists", lists.len());
            assert!(lists.last().unwrap().1.partial);
            let cache: HashMap<Option<u64>, Arc<Children>> = lists.into_iter().map(|(o, c)| (o, Arc::new(c))).collect();
            let path = path_at(&d, d.len() / 2, &[], &mut |o| cache.get(&o).cloned()).unwrap();
            assert!(path.len() <= DEPTH_MAX + 1);
        }
        // up to the limit, as deep as it goes
        let s = format!("{}1{}", "[".repeat(DEPTH_MAX), "]".repeat(DEPTH_MAX));
        let d = doc(&s);
        let lists = scan(&d, None, u64::MAX, Some(DEPTH_MAX as u64), None);
        assert!(!lists.last().unwrap().1.partial);
    }

    #[test]
    fn huge_lists_are_thinned_but_complete() {
        // 300,000 numbers in an array and an object with 150,000 keys.
        let n = 300_000u64;
        let mut s = String::from("{\"nums\": [");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&(i * 7 % 1000).to_string());
        }
        s.push_str("], \"obj\": {");
        for i in 0..150_000 {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("\"k{i}\": [{i}]"));
        }
        s.push_str("}}");
        let d = doc(&s);
        let lists: HashMap<Option<u64>, Children> = scan(&d, None, 1 << 20, None, None).into_iter().collect();
        let root = children(&d, Some(0), None);
        let nums = root.get(&d, 0).unwrap();
        let obj = root.get(&d, 1).unwrap();
        for (open, expect) in [(nums.start, n), (obj.start, 150_000)] {
            let fast = &lists[&Some(open)];
            assert_eq!(fast.count, expect);
            assert!(fast.stride > 1 && (fast.items.len() as u64) < expect / 32, "thinned");
            let simple = children_simple(&d, Some(open), u64::MAX, None);
            assert_eq!(all(&d, fast), simple.items);
            // random access and lookups by position agree with the full list
            for i in [0, 1, 63, 64, 65, 12_345, expect - 1] {
                let c = simple.items[i as usize];
                assert_eq!(fast.get(&d, i), Some(c));
                assert_eq!(fast.find(&d, c.start), Some((i, c)));
                assert_eq!(fast.find(&d, c.first()), Some((i, c)));
                assert_eq!(fast.find_start(&d, c.start), Some(c));
            }
            assert_eq!(fast.range(&d, 100, 230), simple.items[100..230].to_vec());
        }
        // paths into thinned lists
        let mut cache: HashMap<Option<u64>, Arc<Children>> = lists.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
        cache.insert(None, Arc::new(children(&d, None, None)));
        cache.insert(Some(0), Arc::new(root));
        let target = s.find("\"k123456\"").unwrap() as u64 + 3;
        let p = loop {
            match path_at(&d, target, &[], &mut |o| cache.get(&o).cloned()) {
                Ok(p) => break p,
                Err((o, _)) => {
                    let ch = children(&d, o, None);
                    cache.insert(o, Arc::new(ch));
                }
            }
        };
        assert_eq!(path_string(&p), "obj.k123456");
        // JSON Lines with many records
        let lines: String = (0..200_000).map(|i| format!("{{\"i\":{i}}}\n")).collect();
        let d = doc(&lines);
        let top = children(&d, None, None);
        assert_eq!(top.count, 200_000);
        assert!(top.stride > 1);
        let c = top.get(&d, 199_999).unwrap();
        assert_eq!(d.read(c.start, c.end), b"{\"i\":199999}");
    }

    /// `SLATE_SCAN_FILE=<file> cargo test --release --lib scan_speed -- --ignored --nocapture`: how long the outline
    /// scan of a big file takes (in memory).
    #[test]
    #[ignore]
    fn scan_speed() {
        let Some(p) = std::env::var_os("SLATE_SCAN_FILE") else { return };
        let d = Document::from_text(&std::fs::read(p).unwrap());
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let lists = scan(&d, None, 1 << 20, None, None);
            println!("scan: {} lists in {:.1} ms", lists.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
