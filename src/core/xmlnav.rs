//! Lazy XML structure for the path bar and the structure panel, like `jsonnav` for JSON (whose list types it fills
//! in): `children` lists the elements directly inside one element by scanning only that element's bytes, `path_at`
//! finds the path to an offset (`catalog › book[3] › title`), `preview` shows an element's attributes or text.
//! Comments, CDATA sections, processing instructions and the DOCTYPE are skipped. The scan is forgiving: an end tag
//! closes the element it names (and whatever is still open inside it), a stray one is ignored, and in a file cut
//! short whatever is still open ends at the end of the file.

use super::job::Ctx;
use super::jsonnav::{
    Child, Children, Chunked, DENSE_MAX, DEPTH_MAX, Kind, Missing, NESTED_MAX, ORD_UNKNOWN, STRIDE, Step, key_label,
    same_step,
};
use std::sync::Arc;

/// Names are told apart by their first bytes only.
const NAME_MAX: usize = 64;

/// The length of an element's start tag (from its `<` to after its `>`).
pub fn tag_len(c: &Child) -> u64 {
    (c.key_len & 0x7FFF_FFFF) as u64
}

/// Whether an element has elements inside it.
pub fn has_elements(c: &Child) -> bool {
    c.key_len & 0x8000_0000 != 0
}

fn info(tag_len: u64, elements: bool) -> u32 {
    tag_len.min(0x7FFF_FFFF) as u32 | (elements as u32) << 31
}

fn name_hash(name: &[u8]) -> u32 {
    name[..name.len().min(NAME_MAX)].iter().fold(0x811C_9DC5u32, |h, &c| (h ^ c as u32).wrapping_mul(0x0100_0193))
}

fn name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':' || c >= 0x80
}

fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'-' | b'.') || c >= 0x80
}

/// Where the scanner is in the markup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Text,
    /// Right after `<`.
    Lt,
    /// A start tag's name.
    StartName,
    /// A start tag's attributes.
    StartTag,
    /// A quoted attribute value.
    Quote(u8),
    /// An end tag's name, then up to its `>`.
    EndName,
    EndTag,
    /// `<!` and what has been read of it (0: nothing, 1: `-`, 10 + n: n bytes of `[CDATA[`).
    Bang(u8),
    /// A comment; the `-` just before (up to 2).
    Comment(u8),
    /// A CDATA section; the `]` just before (up to 2).
    CData(u8),
    /// A processing instruction; whether a `?` was just before.
    Pi(bool),
    /// `<!DOCTYPE …>` or another declaration: its `[…]` depth, and what in it is being read.
    Decl(u32, Sub),
}

/// What's being read in a declaration: in its `[…]` internal subset, comments and processing instructions are
/// skipped as they are (a quote or a bracket in them is just a character: `<!-- the catalog's elements -->`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    None,
    Quote(u8),
    /// In the subset: `<`, `<!` or `<!-` just read.
    Lt,
    Bang,
    BangDash,
    /// A comment in the subset; the `-` just before (up to 2).
    Comment(u8),
    /// A processing instruction in the subset; whether a `?` was just before.
    Pi(bool),
}

/// An end tag closes the element it names only if that is among the innermost ones this many (a document left so
/// broken is read as well as it can be, and stray end tags in a deep one cost little).
const MAX_UNCLOSED: usize = 1024;

const HASH_START: u32 = 0x811C_9DC5;

fn hash_step(h: u32, c: u8) -> u32 {
    (h ^ c as u32).wrapping_mul(0x0100_0193)
}

/// An element being read.
struct Level {
    /// Its `<` (None: the document, or the place a rescan started).
    open: Option<u64>,
    /// Its name's hash (end tags are matched by it).
    name: u32,
    tag_len: u64,
    /// Where its children start in the shared lists.
    items_from: usize,
    count: u64,
    sparse: bool,
    dense: bool,
    nested: bool,
    first_name: Option<u32>,
    same_names: bool,
}

impl Level {
    fn new(open: Option<u64>, name: u32, tag_len: u64, items_from: usize, nested: bool) -> Level {
        Level {
            open,
            name,
            tag_len,
            items_from,
            count: 0,
            sparse: false,
            dense: false,
            nested,
            first_name: None,
            same_names: true,
        }
    }

    fn list(&self, items: Vec<Child>, names: Vec<u32>, end: u64, content_end: u64, partial: bool) -> Children {
        Children {
            items,
            count: self.count,
            stride: if self.sparse { STRIDE } else { 1 },
            is_object: false,
            end,
            partial,
            xml: true,
            names,
            same_names: self.same_names,
            content_end,
        }
    }
}

struct Scanner {
    at: At,
    stack: Vec<Level>,
    items: Vec<Child>,
    names: Vec<u32>,
    out: Vec<(Option<u64>, Children)>,
    /// The tag being read: where its `<` is, its name's hash, and how many bytes of its name went into it.
    tag_start: u64,
    tag_hash: u32,
    tag_name_len: usize,
    /// The last byte of the chunk before (for a `/>` cut between chunks).
    last: u8,
    keep: u64,
    limit: u64,
    /// The lists of the elements whose content holds this offset are kept too (the path to it).
    path_to: Option<u64>,
    /// The element scanned (None: the document).
    scan_open: Option<u64>,
    /// Scanning one element (not the document, nor a rescan): it is opened by the first start tag.
    root: bool,
    /// Where the scanned element ends, once it has, and where its content ends (its end tag's `<`).
    end: Option<u64>,
    content_end: u64,
    broken: bool,
    /// Elements open inside one too deep to follow (`DEPTH_MAX`), and that one's `<`, start tag length and name.
    deep: u64,
    deep_start: u64,
    deep_tag_len: u64,
    deep_name: u32,
}

impl Scanner {
    fn new(open: Option<u64>, keep: u64, limit: u64, path_to: Option<u64>) -> Scanner {
        Scanner {
            at: At::Text,
            stack: Vec::new(),
            items: Vec::new(),
            names: Vec::new(),
            out: Vec::new(),
            tag_start: open.unwrap_or(0),
            tag_hash: HASH_START,
            tag_name_len: 0,
            last: 0,
            keep,
            limit,
            path_to,
            scan_open: open,
            root: false,
            end: None,
            content_end: 0,
            broken: false,
            deep: 0,
            deep_start: 0,
            deep_tag_len: 0,
            deep_name: 0,
        }
    }

    fn add_child(&mut self, c: Child, h: u32) {
        let lv = self.stack.last_mut().unwrap();
        if !lv.sparse && !lv.dense && lv.count >= if lv.nested { NESTED_MAX } else { DENSE_MAX } {
            let from = lv.items_from;
            let mut w = from;
            for k in (from..self.items.len()).step_by(STRIDE as usize) {
                self.items[w] = self.items[k];
                self.names[w] = self.names[k];
                w += 1;
            }
            self.items.truncate(w);
            self.names.truncate(w);
            lv.sparse = true;
        }
        if !lv.sparse || lv.count % STRIDE == 0 {
            self.items.push(c);
            self.names.push(h);
        }
        match lv.first_name {
            None => lv.first_name = Some(h),
            Some(f) if f != h => lv.same_names = false,
            _ => {}
        }
        lv.count += 1;
    }

    /// The top element ends at `end` (its content at `content_end`): it becomes a child of the one around it (its
    /// list kept if it's big, or holds `path_to`).
    fn finish(&mut self, end: u64, content_end: u64, partial: bool) {
        let lv = self.stack.pop().unwrap();
        let open = lv.open.unwrap_or(0);
        let names = self.names.split_off(lv.items_from);
        let items = self.items.split_off(lv.items_from);
        if end - open >= self.keep || self.path_to.is_some_and(|p| open + lv.tag_len <= p && p < end) {
            self.out.push((lv.open, lv.list(items, names, end, content_end, partial)));
        }
        let c = Child { start: open, end, key_back: 0, key_len: info(lv.tag_len, lv.count > 0) };
        self.add_child(c, lv.name);
    }

    /// A start tag ended at `end` (after its `>`).
    fn start_tag(&mut self, end: u64, empty: bool) {
        let tag_len = end - self.tag_start;
        if self.root {
            // the element being scanned
            self.root = false;
            self.stack.push(Level::new(Some(self.tag_start), self.tag_hash, tag_len, 0, false));
            if empty {
                (self.end, self.content_end) = (Some(end), end);
            }
            return;
        }
        if self.deep > 0 {
            // (inside an element too deep to follow: only its end matters)
            self.deep += !empty as u64;
        } else if empty {
            let c = Child { start: self.tag_start, end, key_back: 0, key_len: info(tag_len, false) };
            self.add_child(c, self.tag_hash);
        } else if self.stack.len() > DEPTH_MAX {
            // Too deep to follow: it becomes a child of the deepest element followed, its insides skipped. Every level
            // costs memory, and a file of nothing but `<a>` would otherwise take about 100 bytes a byte.
            (self.deep, self.deep_start, self.deep_tag_len, self.deep_name) = (1, self.tag_start, tag_len, self.tag_hash);
        } else {
            let from = self.items.len();
            self.stack.push(Level::new(Some(self.tag_start), self.tag_hash, tag_len, from, true));
        }
    }

    /// The element too deep to follow ends at `end`: a child of the deepest one followed.
    fn deep_done(&mut self, end: u64) {
        self.deep = 0;
        let c = Child { start: self.deep_start, end, key_back: 0, key_len: info(self.deep_tag_len, true) };
        self.add_child(c, self.deep_name);
    }

    /// An end tag (from `self.tag_start`, ending at `end`) closes the element it names, and what's open inside it.
    fn end_tag(&mut self, end: u64) {
        if self.deep > 0 {
            // (any end tag: what's that deep is taken to be well-formed)
            self.deep -= 1;
            if self.deep == 0 {
                self.deep_done(end);
            }
            return;
        }
        let h = self.tag_hash;
        let found = self.stack.iter().rev().take(MAX_UNCLOSED).position(|l| l.open.is_some() && l.name == h);
        let Some(d) = found.map(|k| self.stack.len() - 1 - k) else {
            // a stray end tag (in the document itself: it isn't well-formed)
            self.broken |= self.stack.len() == 1 && self.stack[0].open.is_none();
            return;
        };
        while self.stack.len() > d + 1 {
            self.finish(self.tag_start, self.tag_start, true);
        }
        if d == 0 {
            (self.end, self.content_end) = (Some(end), self.tag_start);
        } else {
            self.finish(end, self.tag_start, false);
        }
    }

    /// Reads a chunk at `base`; false once the scan is over.
    fn feed(&mut self, chunk: &[u8], base: u64) -> bool {
        let n = chunk.len();
        let mut i = 0;
        while i < n {
            let c = chunk[i];
            let p = base + i as u64;
            match self.at {
                At::Text => match memchr::memchr(b'<', &chunk[i..]) {
                    Some(k) => {
                        self.tag_start = p + k as u64;
                        self.at = At::Lt;
                        i += k + 1;
                        continue;
                    }
                    None => return true,
                },
                At::Lt => {
                    (self.tag_hash, self.tag_name_len) = (HASH_START, 0);
                    self.at = match c {
                        b'/' => At::EndName,
                        b'!' => At::Bang(0),
                        b'?' => At::Pi(false),
                        b'<' => {
                            self.tag_start = p;
                            At::Lt
                        }
                        c if name_start(c) => {
                            (self.tag_hash, self.tag_name_len) = (hash_step(HASH_START, c), 1);
                            At::StartName
                        }
                        _ => At::Text,
                    };
                }
                At::StartName | At::EndName => {
                    // (names are short: byte by byte)
                    let k = chunk[i..].iter().take_while(|&&b| name_char(b)).count();
                    for &b in &chunk[i..i + k] {
                        if self.tag_name_len < NAME_MAX {
                            self.tag_hash = hash_step(self.tag_hash, b);
                            self.tag_name_len += 1;
                        }
                    }
                    i += k;
                    if i < n {
                        self.at = if self.at == At::StartName { At::StartTag } else { At::EndTag };
                    }
                    continue;
                }
                At::StartTag => match memchr::memchr3(b'"', b'\'', b'>', &chunk[i..]) {
                    Some(k) if chunk[i + k] == b'>' => {
                        // `/>` ends an empty element
                        let before = if i + k > 0 { chunk[i + k - 1] } else { self.last };
                        self.at = At::Text;
                        self.start_tag(p + k as u64 + 1, before == b'/');
                        i += k;
                    }
                    Some(k) => {
                        self.at = At::Quote(chunk[i + k]);
                        i += k + 1;
                        continue;
                    }
                    None => return true,
                },
                At::Quote(q) => match memchr::memchr(q, &chunk[i..]) {
                    Some(k) => {
                        self.at = At::StartTag;
                        i += k + 1;
                        continue;
                    }
                    None => return true,
                },
                At::EndTag => match memchr::memchr(b'>', &chunk[i..]) {
                    Some(k) => {
                        self.at = At::Text;
                        self.end_tag(p + k as u64 + 1);
                        i += k;
                    }
                    None => return true,
                },
                At::Bang(k) => {
                    self.at = match (k, c) {
                        (0, b'-') => At::Bang(1),
                        (1, b'-') => At::Comment(0),
                        (0, b'[') => At::Bang(10),
                        (10..=15, c) if c == b"CDATA["[k as usize - 10] => {
                            if k == 15 {
                                At::CData(0)
                            } else {
                                At::Bang(k + 1)
                            }
                        }
                        _ => {
                            self.at = At::Decl(0, Sub::None);
                            continue;
                        }
                    };
                }
                At::Comment(d) | At::CData(d) => {
                    let mark = if matches!(self.at, At::Comment(_)) { b'-' } else { b']' };
                    let Some(k) = memchr::memchr2(mark, b'>', &chunk[i..]) else {
                        self.at = if mark == b'-' { At::Comment(0) } else { At::CData(0) };
                        return true;
                    };
                    let d = if k > 0 { 0 } else { d };
                    let b = chunk[i + k];
                    let next = if b == mark {
                        Some((d + 1).min(2))
                    } else if d >= 2 {
                        None
                    } else {
                        Some(0)
                    };
                    self.at = match (next, mark) {
                        (None, _) => At::Text,
                        (Some(d), b'-') => At::Comment(d),
                        (Some(d), _) => At::CData(d),
                    };
                    i += k + 1;
                    continue;
                }
                At::Pi(q) => {
                    self.at = match c {
                        b'>' if q => At::Text,
                        b'?' => At::Pi(true),
                        _ => At::Pi(false),
                    };
                }
                At::Decl(depth, sub) => {
                    let decl = |sub| At::Decl(depth, sub);
                    self.at = match (sub, c) {
                        (Sub::Quote(q), _) => decl(if c == q { Sub::None } else { sub }),
                        (Sub::Comment(2), b'>') | (Sub::Pi(true), b'>') => decl(Sub::None),
                        (Sub::Comment(d), b'-') => decl(Sub::Comment((d + 1).min(2))),
                        (Sub::Comment(_), _) => decl(Sub::Comment(0)),
                        (Sub::Pi(_), _) => decl(Sub::Pi(c == b'?')),
                        (Sub::Lt, b'!') => decl(Sub::Bang),
                        (Sub::Lt, b'?') => decl(Sub::Pi(false)),
                        (Sub::Bang, b'-') => decl(Sub::BangDash),
                        (Sub::BangDash, b'-') => decl(Sub::Comment(0)),
                        (Sub::Lt | Sub::Bang | Sub::BangDash, _) => {
                            // another declaration (`<!ELEMENT …>`): this byte is read as part of it
                            self.at = decl(Sub::None);
                            continue;
                        }
                        (Sub::None, b'"' | b'\'') => decl(Sub::Quote(c)),
                        (Sub::None, b'<') if depth > 0 => decl(Sub::Lt),
                        (Sub::None, b'[') => At::Decl(depth + 1, Sub::None),
                        (Sub::None, b']') => At::Decl(depth.saturating_sub(1), Sub::None),
                        (Sub::None, b'>') if depth == 0 => At::Text,
                        (Sub::None, _) => self.at,
                    };
                }
            }
            if self.end.is_some() || (self.stack.len() == 1 && self.stack[0].count >= self.limit) {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// Scans the element whose `<` is at `open` (None = the whole document) in one pass, up to `end`: where its
/// parent's list says it ends (None: unknown, or the document's end). There whatever is still open ends, as at the
/// end of the file, so an element closed by an end tag of one around it (`<rows><row>a</rows>`) reads the same as
/// in its parent's scan. Returns its child list last, preceded by the lists of the elements inside it that are at
/// least `keep` bytes long or whose content holds `path_to` (all of the path to it, from one scan).
pub fn scan(
    src: &dyn Chunked,
    open: Option<u64>,
    end: Option<u64>,
    keep: u64,
    path_to: Option<u64>,
    ctx: Option<&Ctx>,
) -> Vec<(Option<u64>, Children)> {
    let mut sc = Scanner::new(open, keep, u64::MAX, path_to);
    if open.is_some() {
        // its start tag opens it
        sc.at = At::Lt;
        sc.root = true;
    } else {
        sc.stack.push(Level::new(None, 0, 0, 0, false));
    }
    let start = open.map_or(0, |o| o + 1);
    run(&mut sc, src, start, end.unwrap_or(u64::MAX), ctx)
}

/// Up to `count` elements at one level from `from` (where one starts), in a list whose content ends at `bound`.
pub fn rescan(src: &dyn Chunked, from: u64, count: u64, bound: u64) -> Vec<Child> {
    let mut sc = Scanner::new(None, u64::MAX, count, None);
    let mut lv = Level::new(None, 0, 0, 0, false);
    lv.dense = true;
    sc.stack.push(lv);
    run(&mut sc, src, from, bound, None).pop().map(|(_, c)| c.items).unwrap_or_default()
}

fn run(sc: &mut Scanner, src: &dyn Chunked, start: u64, bound: u64, ctx: Option<&Ctx>) -> Vec<(Option<u64>, Children)> {
    let total = src.total().min(bound);
    let mut pos = start;
    let mut since_check = 0u64;
    src.chunked(start.min(total), total, &mut |chunk: &[u8]| {
        let more = sc.feed(chunk, pos);
        if let Some(&b) = chunk.last() {
            sc.last = b;
        }
        pos += chunk.len() as u64;
        since_check += chunk.len() as u64;
        if since_check >= 4 << 20 {
            since_check = 0;
            if let Some(c) = ctx {
                c.set(pos);
                if c.cancelled() {
                    return false;
                }
            }
        }
        more
    });
    let cancelled = ctx.is_some_and(|c| c.cancelled());
    if sc.stack.is_empty() {
        // the element to scan never started (there's no element there)
        return vec![(sc.scan_open, Children { xml: true, partial: true, end: total, content_end: total, ..Default::default() })];
    }
    let mut partial = sc.broken || cancelled;
    let end = match sc.end {
        Some(e) => e,
        None => {
            // the file (or the element) ends with elements still open (or the one scanned): they end there
            let unclosed = sc.stack.len() > 1 || sc.stack[0].open.is_some() || sc.deep > 0;
            if sc.deep > 0 && !cancelled {
                sc.deep_done(total);
            }
            if !cancelled {
                while sc.stack.len() > 1 {
                    sc.finish(total, total, true);
                }
                partial |= unclosed && sc.limit == u64::MAX;
            }
            sc.content_end = total;
            total
        }
    };
    let own = sc.stack.get(1).map_or(sc.items.len(), |l| l.items_from);
    sc.items.truncate(own);
    sc.names.truncate(own);
    let (items, names) = (std::mem::take(&mut sc.items), std::mem::take(&mut sc.names));
    let list = sc.stack[0].list(items, names, end, sc.content_end, partial);
    let mut out = std::mem::take(&mut sc.out);
    out.push((sc.scan_open, list));
    out
}

/// Lists the elements directly inside the element whose `<` is at `open` and which ends at `end` (see `scan`); with
/// `open == None`, the top-level elements of the document (normally one).
pub fn children(src: &dyn Chunked, open: Option<u64>, end: Option<u64>, ctx: Option<&Ctx>) -> Children {
    scan(src, open, end, u64::MAX, None, ctx).pop().map(|(_, c)| c).unwrap_or_default()
}

/// The name of the element whose `<` is at `start`.
pub fn name_at(src: &dyn Chunked, start: u64) -> String {
    let total = src.total();
    let head = src.bytes((start + 1).min(total), (start + 1 + NAME_MAX as u64 * 4).min(total));
    let n = head.iter().take_while(|&&b| name_char(b)).count();
    String::from_utf8_lossy(&head[..n]).into_owned()
}

/// Which of its siblings of the same name an element (child `i` of `ch`) is, from 1; 0 when none has its name.
fn ordinal(ch: &Children, i: u64, name: &str) -> u64 {
    let h = name_hash(name.as_bytes());
    if ch.stride <= 1 {
        let total = ch.names.iter().filter(|&&k| k == h).count();
        if total <= 1 {
            return 0;
        }
        return ch.names[..(i as usize).min(ch.names.len())].iter().filter(|&&k| k == h).count() as u64 + 1;
    }
    // a huge list: only known when all have one name
    if ch.same_names { i + 1 } else { ORD_UNKNOWN }
}

/// The path to `offset`: the elements around it, from the outermost. `get(open)` returns the cached children of
/// an element (None = the document); when one isn't known yet this returns `Err` with it (and where it ends) so the
/// caller can scan it and ask again. `prev` is the path worked out last for the same text: where this one goes the
/// same way, its steps (names, places among their siblings) are taken as they are.
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
        let step = match same_step(prev, path.len(), open, i, c.start) {
            Some(s) => Step { end: c.end, ..s.clone() },
            None => {
                let name = name_at(src, c.start);
                let ord = ordinal(&ch, i, &name);
                Step { index: i, key: Some(name), start: c.start, end: c.end, parent: open, ord }
            }
        };
        path.push(step);
        // in its content (past the start tag) with elements in it: go in (no deeper than a scan follows)
        if has_elements(&c) && offset >= c.start + tag_len(&c) && offset < c.end && path.len() < DEPTH_MAX {
            (open, end) = (Some(c.start), c.end);
            continue;
        }
        break;
    }
    Ok(path)
}

/// What the structure panel shows after an element's name: its attributes, and for one without elements inside its
/// text.
pub fn preview(src: &dyn Chunked, c: &Child, max: usize) -> (Kind, String) {
    let kind = if has_elements(c) { Kind::Element } else { Kind::Leaf };
    let tl = tag_len(c);
    let head = src.bytes(c.start, (c.start + tl.min(4096)).min(c.end));
    let mut out = String::new();
    // name="value" pairs after the name
    let mut i = 1 + head[1.min(head.len())..].iter().take_while(|&&b| name_char(b)).count();
    while i < head.len() && out.chars().count() < max {
        let b = head[i];
        if name_start(b) {
            let e = i + head[i..].iter().take_while(|&&b| name_char(b)).count();
            let mut j = e + head[e..].iter().take_while(|b| b.is_ascii_whitespace()).count();
            if head.get(j) == Some(&b'=') {
                j += 1;
                j += head[j..].iter().take_while(|b| b.is_ascii_whitespace()).count();
                let q = head.get(j).copied().unwrap_or(0);
                if q == b'"' || q == b'\'' {
                    let v = head[j + 1..].iter().position(|&b| b == q).map_or(head.len(), |p| j + 1 + p);
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    let value = squeeze(&head[j + 1..v]);
                    out.push_str(&format!("{}=\"{}\"", String::from_utf8_lossy(&head[i..e]), cut(&value, 40)));
                    i = v + 1;
                    continue;
                }
            }
            i = e;
        } else {
            i += 1;
        }
    }
    if kind == Kind::Leaf && tl < c.end - c.start {
        let body = src.bytes(c.start + tl, (c.start + tl + 4096).min(c.end));
        let body = body.strip_prefix(b"<![CDATA[").unwrap_or(&body);
        let text = squeeze(&body[..body.iter().position(|&b| b == b'<').unwrap_or(body.len())]);
        if !text.is_empty() {
            if !out.is_empty() {
                out.push_str("  ");
            }
            out.push_str(&text);
        }
    }
    (kind, cut(&key_label(&out), max))
}

/// Text with its runs of whitespace as one space, trimmed.
fn squeeze(b: &[u8]) -> String {
    String::from_utf8_lossy(b).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

/// The path as XPath: `/catalog/book[3]/title` (an element whose place among its siblings isn't known by name is
/// `*[n]`, its place among all of them).
pub fn xpath(path: &[Step]) -> String {
    let mut s = String::new();
    for st in path {
        let name = st.key.as_deref().unwrap_or("*");
        match st.ord {
            0 => s.push_str(&format!("/{name}")),
            ORD_UNKNOWN => s.push_str(&format!("/*[{}]", st.index + 1)),
            n => s.push_str(&format!("/{name}[{n}]")),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use std::collections::HashMap;

    /// Bytes handed out in chunks of `k` (so tags, comments and names are cut at every place).
    struct Bits(Vec<u8>, usize);

    impl Chunked for Bits {
        fn total(&self) -> u64 {
            self.0.len() as u64
        }
        fn chunked(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
            let b = (b as usize).min(self.0.len());
            let mut p = a as usize;
            while p < b {
                let e = (p + self.1).min(b);
                if !f(&self.0[p..e]) {
                    return false;
                }
                p = e;
            }
            true
        }
    }

    fn names(src: &dyn Chunked, ch: &Children) -> Vec<String> {
        ch.range(src, 0, ch.count).iter().map(|c| name_at(src, c.start)).collect()
    }

    fn path(src: &dyn Chunked, off: u64) -> String {
        let mut cache: HashMap<Option<u64>, Arc<Children>> = HashMap::new();
        loop {
            match path_at(src, off, &[], &mut |o| cache.get(&o).cloned()) {
                Ok(p) => return p.iter().map(|s| s.label()).collect::<Vec<_>>().join(" › "),
                Err((o, end)) => {
                    cache.insert(o, Arc::new(children(src, o, Some(end), None)));
                }
            }
        }
    }

    const DOC: &str = "<?xml version=\"1.0\"?>\n<!DOCTYPE catalog [<!ELEMENT catalog ANY>]>\n<!-- a <comment> -->\n<catalog xmlns=\"urn:x\">\n  <book id=\"1\" note='a > b'><title>XML <i>Guide</i></title><price>4</price></book>\n  <book id=\"2\"><title><![CDATA[<raw>]]></title><empty/></book>\n  <?pi x?>\n  <magazine/>\n</catalog>\n";

    #[test]
    fn nesting_too_deep_to_follow_is_one_element_and_costs_little() {
        let deep = DEPTH_MAX + 1000;
        // never closed (a file of nothing but `<a>`), and closed with an element after it
        for s in ["<a>".repeat(deep * 2), format!("<r>{}x{}<after/></r>", "<a>".repeat(deep), "</a>".repeat(deep))] {
            let src = Bits(s.into_bytes(), 1 << 20);
            let lists = scan(&src, None, None, u64::MAX, Some(src.total() / 2), None);
            assert!(lists.len() <= DEPTH_MAX + 3, "{} lists", lists.len());
        }
        let s = format!("<r>{}x{}<after/></r>", "<a>".repeat(deep), "</a>".repeat(deep));
        let src = Bits(s.into_bytes(), 1 << 20);
        let top = children(&src, None, None, None);
        let r = children(&src, Some(top.items[0].start), None, None);
        assert_eq!(names(&src, &r), ["a", "after"]);
        let at = src.total() / 2;
        let cache: HashMap<Option<u64>, Arc<Children>> =
            scan(&src, None, None, u64::MAX, Some(at), None).into_iter().map(|(o, c)| (o, Arc::new(c))).collect();
        let p = path_at(&src, at, &[], &mut |o| cache.get(&o).cloned()).unwrap();
        assert!(p.len() <= DEPTH_MAX + 2, "{}", p.len());
    }

    #[test]
    fn lists_elements_and_skips_the_rest() {
        for k in [1, 2, 3, 7, 64, 1 << 20] {
            let src = Bits(DOC.as_bytes().to_vec(), k);
            let top = children(&src, None, None, None);
            assert_eq!(names(&src, &top), ["catalog"], "chunks of {k}");
            assert!(!top.partial && top.xml);
            let root = top.items[0];
            assert!(has_elements(&root) && root.end == DOC.find("</catalog>").unwrap() as u64 + 10);
            let cat = children(&src, Some(root.start), None, None);
            assert_eq!(names(&src, &cat), ["book", "book", "magazine"], "chunks of {k}");
            assert!(!cat.same_names);
            let book = cat.items[0];
            assert_eq!(tag_len(&book), "<book id=\"1\" note='a > b'>".len() as u64);
            assert_eq!(names(&src, &children(&src, Some(book.start), None, None)), ["title", "price"]);
            let book2 = children(&src, Some(cat.items[1].start), None, None);
            assert_eq!(names(&src, &book2), ["title", "empty"]);
            assert!(!has_elements(&book2.items[0]) && !has_elements(&book2.items[1]));
            assert!(!has_elements(&cat.items[2]) && cat.items[2].end - cat.items[2].start == 11);
        }
    }

    #[test]
    fn paths_and_xpaths() {
        let d = Document::from_text(DOC.as_bytes());
        let at = |s: &str| DOC.find(s).unwrap() as u64;
        assert_eq!(path(&d, at("XML <i>")), "catalog › book[1] › title");
        assert_eq!(path(&d, at("Guide")), "catalog › book[1] › title › i");
        assert_eq!(path(&d, at("<price>") + 2), "catalog › book[1] › price");
        assert_eq!(path(&d, at("<raw>")), "catalog › book[2] › title");
        assert_eq!(path(&d, at("<magazine")), "catalog › magazine");
        assert_eq!(path(&d, at("xmlns")), "catalog");
        assert_eq!(path(&d, 3), "");
        let mut cache: HashMap<Option<u64>, Arc<Children>> = HashMap::new();
        let p = loop {
            match path_at(&d, at("<empty"), &[], &mut |o| cache.get(&o).cloned()) {
                Ok(p) => break p,
                Err((o, end)) => {
                    cache.insert(o, Arc::new(children(&d, o, Some(end), None)));
                }
            }
        };
        assert_eq!(xpath(&p), "/catalog/book[2]/empty");
        let book = &p[1];
        assert_eq!(preview(&d, &Child { start: book.start, end: book.end, key_back: 0, key_len: info(14, true) }, 80), (Kind::Element, "id=\"2\"".into()));
        let cat = cache[&Some(p[0].start)].clone();
        assert_eq!(preview(&d, &children(&d, Some(cat.items[0].start), None, None).items[1], 80), (Kind::Leaf, "4".into()));
    }

    #[test]
    fn broken_and_cut_short() {
        // an end tag closes what it names and what's open inside it; a stray one is ignored
        let src = "<a><b><c>x</a><d/></e>";
        let d = Document::from_text(src.as_bytes());
        let top = children(&d, None, None, None);
        assert_eq!(names(&d, &top), ["a", "d"]);
        let a = children(&d, Some(0), None, None);
        assert_eq!(names(&d, &a), ["b"]);
        assert_eq!(a.items[0].end, src.find("</a>").unwrap() as u64);
        assert!(top.partial);
        // cut short: what's open ends at the end
        let src = "<r><x>1</x><y><z";
        let d = Document::from_text(src.as_bytes());
        let r = children(&d, Some(0), None, None);
        assert_eq!(names(&d, &r), ["x", "y"]);
        assert!(r.partial && r.items[1].end == src.len() as u64);
        assert_eq!(path(&d, 14), "r › y");
    }

    /// Random markup: the same lists whatever the chunks, paths and previews anywhere, nothing that panics.
    #[test]
    fn odd_markup_reads_the_same_in_any_chunks() {
        let parts: &[&str] = &[
            "<a>", "</a>", "<b x=\"1\">", "</b>", "<c/>", "<a ", "/>", "<!--", "-->", "<![CDATA[", "]]>", "<?", "?>",
            "<!DOCTYPE x [", "]>", "\"", "'", ">", "<", "</", "text", " ", "\n", "-", "]", "?", "<d k='>'>", "</d>",
        ];
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        for _ in 0..300 {
            let mut s = String::new();
            for _ in 0..next() % 40 {
                s.push_str(parts[(next() % parts.len() as u64) as usize]);
            }
            let whole = Bits(s.as_bytes().to_vec(), 1 << 20);
            let lists = |src: &dyn Chunked, open| {
                let ch = children(src, open, None, None);
                (ch.count, ch.items.clone(), ch.names.clone(), ch.end, ch.partial)
            };
            let top = children(&whole, None, None, None);
            let mut opens = vec![None];
            opens.extend(top.items.iter().map(|c| Some(c.start)));
            for k in [1, 2, 3, 5] {
                let bits = Bits(s.as_bytes().to_vec(), k);
                for &o in &opens {
                    assert_eq!(lists(&bits, o), lists(&whole, o), "chunks of {k}, element at {o:?} in {s:?}");
                }
            }
            for off in 0..=s.len() as u64 {
                path(&whole, off);
            }
            for c in &top.items {
                preview(&whole, c, 80);
            }
        }
    }

    #[test]
    fn comments_and_pis_in_the_doctype_hide_their_quotes() {
        for subset in ["<!-- the catalog's elements -->", "<?pi don't?>", "<!ENTITY co \"Acme ]> Inc\">", "<!-- a [ b > -->"] {
            let s = format!("<!DOCTYPE catalog [ {subset} <!ELEMENT catalog ANY> ]>\n<catalog><book/></catalog>\n");
            for k in [1, 2, 3, 1 << 20] {
                let src = Bits(s.as_bytes().to_vec(), k);
                let top = children(&src, None, None, None);
                assert_eq!(names(&src, &top), ["catalog"], "{s}");
                let cat = top.items[0];
                assert_eq!(names(&src, &children(&src, Some(cat.start), Some(cat.end), None)), ["book"], "{s}");
            }
        }
    }

    #[test]
    fn an_element_closed_by_an_outer_end_tag_reads_the_same_on_its_own() {
        // the second <row> is closed by </rows>: scanned on its own it mustn't take in what comes after
        let s = "<doc><rows><row>a</row><row><x/></rows><other/><more/></doc>";
        let d = Document::from_text(s.as_bytes());
        let el = children(&d, None, None, None).items[0];
        let inside = children(&d, Some(el.start), Some(el.end), None);
        assert_eq!(names(&d, &inside), ["rows", "other", "more"]);
        let rows = inside.items[0];
        let list = children(&d, Some(rows.start), Some(rows.end), None);
        assert_eq!(names(&d, &list), ["row", "row"]);
        let row = list.items[1];
        assert_eq!(row.end, s.find("</rows>").unwrap() as u64);
        assert_eq!(names(&d, &children(&d, Some(row.start), Some(row.end), None)), ["x"]);
        assert_eq!(path(&d, s.find("<x/>").unwrap() as u64 + 1), "doc › rows › row[2] › x");
        // a huge list whose last element is closed by the list's end tag: read again from the 64th element before,
        // it ends there too, not at the end of the file
        let s = format!("<rows>{}<row>b</rows><z/>", "<row>a</row>".repeat(150_000));
        let d = Document::from_text(s.as_bytes());
        let top = children(&d, None, None, None);
        assert_eq!(names(&d, &top), ["rows", "z"]);
        let rows = children(&d, Some(0), Some(top.items[0].end), None);
        assert_eq!((rows.count, rows.stride), (150_001, STRIDE));
        let last = rows.get(&d, 150_000).unwrap();
        assert_eq!((last.end, has_elements(&last)), (s.find("</rows>").unwrap() as u64, false));
    }

    /// Malformed markup: an element scanned on its own, and its children scanned again from any of them, read as in
    /// the scan of the whole document.
    #[test]
    fn scans_of_parts_agree_with_the_whole() {
        let parts = ["<a>", "</a>", "<b>", "</b>", "<c>", "</c>", "</d>", "<e/>", "x", "<a k='>'>", "<!-- </a> -->"];
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        for _ in 0..3000 {
            let s: String = (0..next() % 30).map(|_| parts[(next() % parts.len() as u64) as usize]).collect();
            let d = Bits(s.as_bytes().to_vec(), 1 << 20);
            let lists = scan(&d, None, None, 0, None, None);
            for (open, l) in &lists {
                let what = |c: &Children| (c.items.clone(), c.count, c.names.clone(), c.end, c.content_end);
                if let Some(o) = *open {
                    assert_eq!(what(&children(&d, Some(o), Some(l.end), None)), what(l), "element at {o} in {s}");
                }
                for k in 0..l.items.len() {
                    let again = rescan(&d, l.items[k].start, l.count - k as u64, l.content_end);
                    assert_eq!(again, l.items[k..], "from child {k} of {open:?} in {s}");
                }
            }
        }
    }

    #[test]
    fn deep_and_stray_tags_cost_little() {
        let n = 200_000;
        let s = format!("<r>{}{}</r>", "<a>".repeat(n), "</b>".repeat(n));
        let d = Document::from_text(s.as_bytes());
        let start = std::time::Instant::now();
        let top = children(&d, None, None, None);
        assert_eq!(top.count, 1);
        assert!(start.elapsed().as_secs_f64() < 2.0, "{:?}", start.elapsed());
    }

    #[test]
    fn huge_lists_keep_every_64th() {
        let mut s = String::from("<rows>");
        for i in 0..150_000 {
            s.push_str(if i % 2 == 0 || i < 120_000 { "<row/>" } else { "<other/>" });
        }
        s.push_str("</rows>");
        let d = Document::from_text(s.as_bytes());
        let rows = children(&d, Some(0), None, None);
        assert_eq!(rows.count, 150_000);
        assert_eq!(rows.stride, STRIDE);
        assert!(!rows.same_names);
        let c = rows.get(&d, 130_001).unwrap();
        assert_eq!(name_at(&d, c.start), "other");
        // a huge list of mixed names: the place among all is used
        let off = c.start + 2;
        let mut cache: HashMap<Option<u64>, Arc<Children>> = HashMap::new();
        let p = loop {
            match path_at(&d, off, &[], &mut |o| cache.get(&o).cloned()) {
                Ok(p) => break p,
                Err((o, end)) => {
                    cache.insert(o, Arc::new(children(&d, o, Some(end), None)));
                }
            }
        };
        assert_eq!(xpath(&p), "/rows/*[130002]");
        // all of one name: known
        let s = format!("<rows>{}</rows>", "<row>x</row>".repeat(120_000));
        let d = Document::from_text(s.as_bytes());
        assert_eq!(path(&d, s.len() as u64 - 12), "rows › row[120000]");
    }

    /// `SLATE_SCAN_FILE=<file> cargo test --release --lib xml_scan_speed -- --ignored --nocapture`: how long the
    /// outline scan of a big XML file takes (in memory).
    #[test]
    #[ignore]
    fn xml_scan_speed() {
        let Some(p) = std::env::var_os("SLATE_SCAN_FILE") else { return };
        let d = Document::from_text(&std::fs::read(p).unwrap());
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let lists = scan(&d, None, None, 1 << 20, None, None);
            println!("xml scan: {} lists in {:.1} ms", lists.len(), t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
