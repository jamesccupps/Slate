//! Find and replace over documents of any size. The text is searched in windows of a few MB (zero-copy when a
//! window sits in one in-memory piece), so even multi-GB files stream through at disk speed. Every query becomes
//! a byte regex (plain text is escaped). Plain text without Whole word is helped along, finding exactly what the
//! regex would (`Literal`): with Match case byte for byte (`memmem`); ignoring case, a query like
//! `"status":"refunded"` (the commonest search in a JSON file) by its rarest-looking part first.
//!
//! A window is searched together with an overlap after it, so a match that starts in it can finish there; one that
//! runs into the end of that is searched again in a bigger window. A match that ends even further on can't be told
//! from no match there (a regex like `"[^"]*"` over a long string), so what's exact (the same matches as one search
//! of the whole text) is:
//! - plain text, always: the overlap is longer than anything the query can match;
//! - a regex in a document up to 64 MiB: the whole text is one window (Find previous too, from its start);
//! - a regex in a bigger one: Count all and Replace all search 64 MiB at a time with 16 MiB after it, so they're
//!   exact while no match is longer than 16 MiB; Find next and previous 8 MB with 1 MiB after it (the first match
//!   comes quickly), exact while none is longer than 1 MiB. Find previous starts its window before the caret, where
//!   a match can be cut in two: the app takes the previous match from the count's list when it has it.
//!
//! A longer match where two windows meet is missed or cut short, and the matches after it can be wrong too: the
//! search goes on from inside it.

use std::io::{self, Write};

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};

use super::buffer::Snapshot;
use super::document::Document;
use super::job::Ctx;
use super::source::IndexBuilder;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// Bytes before a window kept for look-behind (`^`, `\b`).
const CTX: u64 = 16;
/// Extra bytes after a window so matches that start inside it can finish (plain text: as long as the query). The
/// sizes are small in tests, so they cross many seams.
const OVERLAP: u64 = if cfg!(test) { 64 } else { 64 << 10 };
/// The window searched at a time, and the overlap for a regex, whose matches can be of any length (Find next and
/// previous; see the module docs).
const WINDOW: u64 = if cfg!(test) { 1000 } else { 8 << 20 };
const OVERLAP_REGEX: u64 = if cfg!(test) { 256 } else { 1 << 20 };
/// A regex searches documents up to this size in one window (exact).
const ONE_WINDOW: u64 = if cfg!(test) { 4096 } else { 64 << 20 };
/// The window and overlap of Count all and Replace all with a regex in bigger documents.
const BIG_WINDOW: u64 = if cfg!(test) { 2048 } else { 64 << 20 };
const BIG_OVERLAP: u64 = if cfg!(test) { 1500 } else { 16 << 20 };
/// Largest window tried when a match keeps running into the window's end.
const MAX_WINDOW: u64 = 256 << 20;

pub trait Haystack {
    fn hay_len(&self) -> u64;
    /// The bytes `[a, b)`, without copying when possible.
    fn hay<'a>(&'a self, a: u64, b: u64, scratch: &'a mut Vec<u8>) -> &'a [u8];
}

impl Haystack for Snapshot {
    fn hay_len(&self) -> u64 {
        self.len()
    }
    fn hay<'a>(&'a self, a: u64, b: u64, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        self.slice(a, b, scratch)
    }
}

impl Haystack for Document {
    fn hay_len(&self) -> u64 {
        self.len()
    }
    fn hay<'a>(&'a self, a: u64, b: u64, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        scratch.clear();
        self.read_into(a, b, scratch);
        scratch
    }
}

#[derive(Clone)]
pub struct Matcher {
    re: Regex,
    /// Plain text searched without the regex (it finds the same).
    lit: Option<Literal>,
    regex_mode: bool,
    /// Bytes after a window a match that starts in it may need (see `OVERLAP`).
    overlap: u64,
}

/// A plain-text query searched more directly than through its regex, finding exactly what the regex would.
#[derive(Clone)]
enum Literal {
    /// Match case: the bytes as they are.
    Exact(memmem::Finder<'static>),
    /// Ignoring case, an ASCII query whose longest run of letters and digits isn't where it starts
    /// (`"status":"refunded"`): its regex looks for it from its start, which is often far commoner than all of it
    /// (every record's `"status"`), and checks each place. That run is looked for instead (`run`, a regex of its own:
    /// as fast as plain text goes), then the whole query around each place it is (`span`: the most bytes a match can
    /// take, a letter of other case taking up to three times its bytes).
    Anchored { run: Regex, span: usize },
}

impl Literal {
    fn new(q: &Query) -> Option<Literal> {
        if q.regex || q.whole_word {
            return None;
        }
        if q.match_case {
            return Some(Literal::Exact(memmem::Finder::new(q.text.as_bytes()).into_owned()));
        }
        if !q.text.is_ascii() {
            return None;
        }
        let b = q.text.as_bytes();
        let (mut best, mut i) = ((0, 0), 0);
        while i < b.len() {
            let s = i;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            if i - s > best.1 - best.0 {
                best = (s, i);
            }
            i = i.max(s + 1);
        }
        // (where the query starts, or short: the regex does as well)
        if best.0 == 0 || best.1 - best.0 < 3 {
            return None;
        }
        let run = RegexBuilder::new(&regex::escape(&q.text[best.0..best.1])).case_insensitive(true).build().ok()?;
        Some(Literal::Anchored { run, span: 3 * b.len() })
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Matcher {
    pub fn new(q: &Query) -> Result<Matcher, String> {
        if q.text.is_empty() {
            return Err("Nothing to find".into());
        }
        let mut pat = if q.regex { q.text.clone() } else { regex::escape(&q.text) };
        if q.whole_word {
            if q.regex {
                pat = format!(r"\b(?:{pat})\b");
            } else {
                let first = q.text.chars().next().is_some_and(is_word_char);
                let last = q.text.chars().last().is_some_and(is_word_char);
                pat = format!("{}{pat}{}", if first { r"\b" } else { "" }, if last { r"\b" } else { "" });
            }
        }
        let re = RegexBuilder::new(&pat)
            .case_insensitive(!q.match_case)
            .multi_line(true)
            .crlf(true)
            .size_limit(64 << 20)
            .dfa_size_limit(64 << 20)
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(s) => s.lines().last().unwrap_or("Invalid pattern").trim().to_string(),
                regex::Error::CompiledTooBig(_) => "Pattern is too big".into(),
                _ => "Invalid pattern".into(),
            })?;
        // (plain text: what the query can match, a letter of other case taking up to three times its bytes)
        let overlap = if q.regex { OVERLAP_REGEX } else { OVERLAP.max(4 * q.text.len() as u64 + CTX) };
        Ok(Matcher { re, lit: Literal::new(q), regex_mode: q.regex, overlap })
    }

    /// The first match in `hay` starting at `at` or after (the text before `at` counts for `\b` and `^`).
    fn find_at(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        match &self.lit {
            Some(Literal::Exact(f)) => f.find(&hay[at..]).map(|i| (at + i, at + i + f.needle().len())),
            Some(Literal::Anchored { run, span }) => {
                // At each place the run is, the whole query in a window around it: the first match there that starts
                // no later than the run is the first match of all (one that starts later is found at its own run,
                // where it fits in the window whole; see the tests).
                let mut from = at;
                while let Some(r) = run.find_at(hay, from) {
                    let (ws, we) = (r.start().saturating_sub(*span).max(at), (r.start() + 2 * span).min(hay.len()));
                    if let Some(m) = self.re.find_at(&hay[..we], ws).filter(|m| m.start() <= r.start()) {
                        return Some((m.start(), m.end()));
                    }
                    from = r.start() + 1;
                }
                None
            }
            None => self.re.find_at(hay, at).map(|m| (m.start(), m.end())),
        }
    }

    /// How long a text can be to search it on the UI thread: plain text is searched at several GB/s (32 MB in a few
    /// ms), but some regexes go at a few dozen MB/s (`\b\w{30,}\b` over text that isn't all ASCII), which would
    /// freeze the window for seconds.
    pub fn sync_limit(&self) -> u64 {
        if self.regex_mode { 256 << 10 } else { 32 << 20 }
    }

    /// The window and overlap to search a text of `len` bytes with (see the module docs); `all`: for every match
    /// (Count all, Replace all), not only the next one.
    fn sizes(&self, len: u64, all: bool) -> (u64, u64) {
        if !self.regex_mode {
            (WINDOW, self.overlap)
        } else if len <= ONE_WINDOW {
            (ONE_WINDOW, 0)
        } else if all {
            (BIG_WINDOW, BIG_OVERLAP)
        } else {
            (WINDOW, OVERLAP_REGEX)
        }
    }

    /// First match starting in `[from, to)`.
    pub fn find_fwd(&self, h: &dyn Haystack, from: u64, to: u64, ctx: Option<&Ctx>) -> Option<(u64, u64)> {
        let mut found = None;
        self.windows(h, from, to, ctx, self.sizes(h.hay_len(), false), &mut |_, _, hit| match hit {
            Hit::Match(s, e) => {
                found = Some((s, e));
                false
            }
            Hit::Done(_) => true,
        });
        found
    }

    /// Last match that starts at or after `from` and ends at or before `to`. The text after `to` still counts for
    /// whether something is a match (`\b`, `$`): the whole word "foo" isn't in "foobar", wherever the caret is.
    pub fn find_back(&self, h: &dyn Haystack, from: u64, to: u64, ctx: Option<&Ctx>) -> Option<(u64, u64)> {
        let len = h.hay_len();
        let to = to.min(len);
        if self.regex_mode && len <= ONE_WINDOW {
            // A regex's matches depend on where the one before ended: from the start, in one window (exact).
            let mut best = None;
            self.windows(h, 0, to, ctx, self.sizes(len, true), &mut |_, _, hit| {
                if let Hit::Match(s, e) = hit {
                    if s >= from && e <= to {
                        best = Some((s, e));
                    }
                }
                true
            });
            return best;
        }
        let mut win = 1u64 << 20;
        let mut end = to;
        let mut scratch = Vec::new();
        while end > from {
            if ctx.is_some_and(|c| c.cancelled()) {
                return None;
            }
            let start = end.saturating_sub(win).max(from);
            let hs = start.saturating_sub(CTX);
            let he = (end + self.overlap).min(len);
            let hay = h.hay(hs, he, &mut scratch);
            let mut best = None;
            let mut at = (start - hs) as usize;
            while at <= hay.len() {
                let Some(m) = self.find_at(hay, at) else { break };
                let ms = hs + m.0 as u64;
                if ms >= end {
                    break;
                }
                let me = hs + m.1 as u64;
                if me <= to {
                    best = Some((ms, me));
                }
                at = next_at(hay, m);
            }
            if best.is_some() {
                return best;
            }
            end = start;
            win = (win * 2).min(64 << 20);
        }
        None
    }

    /// Calls `f(hay, hay_start, match_start, match_end)` for every match starting in `[from, to)`, in order; stop by
    /// returning false. `hay` holds the match (for captures).
    pub fn each(
        &self,
        h: &dyn Haystack,
        from: u64,
        to: u64,
        ctx: Option<&Ctx>,
        window: u64,
        f: &mut dyn FnMut(&[u8], u64, u64, u64) -> bool,
    ) {
        self.windows(h, from, to, ctx, (window, self.overlap), &mut |hay, hs, hit| match hit {
            Hit::Match(s, e) => f(hay, hs, s, e),
            Hit::Done(_) => true,
        });
    }

    /// `each` with windows of `(window, overlap)` bytes, also telling `f` when a window is done: no match starts
    /// before `Hit::Done`'s offset that wasn't reported (so the text up to there can be taken from that window's
    /// `hay` while it's there).
    fn windows(
        &self,
        h: &dyn Haystack,
        from: u64,
        to: u64,
        ctx: Option<&Ctx>,
        (window, overlap): (u64, u64),
        f: &mut dyn FnMut(&[u8], u64, Hit) -> bool,
    ) {
        let len = h.hay_len();
        let to = to.min(len);
        let mut pos = from;
        let mut win = window;
        let mut scratch = Vec::new();
        let mut last_end: Option<u64> = None;
        while pos < to {
            if let Some(c) = ctx {
                if c.cancelled() {
                    return;
                }
                c.set(pos);
            }
            let hs = pos.saturating_sub(CTX);
            let he = (pos + win + overlap).min(len);
            let limit = if he == len { u64::MAX } else { pos + win };
            let hay = h.hay(hs, he, &mut scratch);
            let mut at = (pos - hs) as usize;
            let mut next = limit.min(to);
            let mut grow = false;
            while at <= hay.len() {
                let Some(m) = self.find_at(hay, at) else { break };
                let ms = hs + m.0 as u64;
                let me = hs + m.1 as u64;
                // An empty match at the very end of the text counts (e.g. `^` after a final line break).
                if ms > to || (ms == to && (to < len || me > ms)) {
                    return;
                }
                if ms >= limit {
                    // The next window's, which searches again from where this one's part ends: a match that starts
                    // in the overlap may run past its end (and then this is one inside it).
                    break;
                }
                if m.1 == hay.len() && he < len {
                    // The match may continue past this window: search again from it with a bigger window.
                    next = ms;
                    grow = win < MAX_WINDOW;
                    if !grow {
                        // Give up on extending; report what we have.
                        if !f(hay, hs, Hit::Match(ms, me)) {
                            return;
                        }
                        next = me.max(ms + 1);
                    }
                    break;
                }
                // Skip an empty match right where the previous match ended.
                if !(m.0 == m.1 && last_end == Some(ms)) && !f(hay, hs, Hit::Match(ms, me)) {
                    return;
                }
                last_end = Some(me);
                at = next_at(hay, m);
                // (not inside a character: after an empty match `at` is past it)
                next = (hs + at as u64).max(limit.min(to));
            }
            if grow {
                win = (win * 4).min(MAX_WINDOW);
            } else {
                win = window;
            }
            if next <= pos && !grow {
                next = pos + 1;
            }
            if !f(hay, hs, Hit::Done(next)) {
                return;
            }
            pos = next;
        }
    }

    /// All matches within `hay` (a small piece of text such as the visible lines), as offsets from `base`.
    pub fn matches_in(&self, hay: &[u8], base: u64, limit: usize) -> Vec<(u64, u64)> {
        if self.lit.is_none() {
            return self
                .re
                .find_iter(hay)
                .filter(|m| m.start() != m.end())
                .take(limit)
                .map(|m| (base + m.start() as u64, base + m.end() as u64))
                .collect();
        }
        // (plain text: never empty)
        let mut v = Vec::new();
        let mut at = 0;
        while v.len() < limit {
            let Some((s, e)) = self.find_at(hay, at) else { break };
            v.push((base + s as u64, base + e as u64));
            at = e;
        }
        v
    }

    /// Whether `[s, e)` of `h` is a match where it is, with the text around it (to decide if the selection is the
    /// current match): with `\b`, `^`, `$` or a run that goes on, a piece of text alone can match where it doesn't.
    pub fn is_match_at(&self, h: &dyn Haystack, s: u64, e: u64) -> bool {
        let hs = s.saturating_sub(CTX);
        let he = (e + CTX).min(h.hay_len());
        let mut scratch = Vec::new();
        let hay = h.hay(hs, he, &mut scratch);
        self.find_at(hay, (s - hs) as usize).is_some_and(|m| hs + m.0 as u64 == s && hs + m.1 as u64 == e)
    }

    /// The replacement for the match at `[s, e)` of `hay` (expands `$1`, `${name}` in regex mode).
    pub fn expand(&self, hay: &[u8], s: usize, replacement: &[u8], out: &mut Vec<u8>) {
        // (without a `$` there's nothing to expand: no need to find the groups)
        if !self.regex_mode || !replacement.contains(&b'$') {
            out.extend_from_slice(replacement);
            return;
        }
        match self.re.captures_at(hay, s) {
            Some(caps) => caps.expand(replacement, out),
            None => out.extend_from_slice(replacement),
        }
    }

    /// Writes the text of `snap` with every match replaced. Returns the number of replacements.
    pub fn replace_all_to(
        &self,
        snap: &Snapshot,
        replacement: &[u8],
        w: &mut dyn Write,
        idx: &mut IndexBuilder,
        ctx: &Ctx,
    ) -> io::Result<u64> {
        let mut count = 0u64;
        let mut copied = 0u64;
        let mut err: Option<io::Error> = None;
        let mut buf = Vec::new();
        let put = |c: &[u8], w: &mut dyn Write, idx: &mut IndexBuilder| {
            idx.push(c);
            w.write_all(c)
        };
        // The text between matches, from the window it was searched in (read again only where it isn't in it).
        let copy = |hay: &[u8], hs: u64, a: u64, b: u64, w: &mut dyn Write, idx: &mut IndexBuilder| {
            if a >= b {
                return Ok(());
            }
            let he = hs + hay.len() as u64;
            if a >= hs && b <= he {
                return put(&hay[(a - hs) as usize..(b - hs) as usize], w, idx);
            }
            let mut e = None;
            snap.chunks(a, b, &mut |c| {
                e = put(c, w, idx).err();
                e.is_none()
            });
            e.map_or(Ok(()), Err)
        };
        self.windows(snap, 0, snap.len(), Some(ctx), self.sizes(snap.len(), true), &mut |hay, hs, hit| {
            let r = match hit {
                Hit::Done(upto) => copy(hay, hs, copied, upto, w, idx).map(|_| copied = copied.max(upto)),
                Hit::Match(s, e) => copy(hay, hs, copied, s, w, idx).and_then(|_| {
                    buf.clear();
                    self.expand(hay, (s - hs) as usize, replacement, &mut buf);
                    copied = e;
                    count += 1;
                    put(&buf, w, idx)
                }),
            };
            if let Err(x) = r {
                err = Some(x);
                return false;
            }
            true
        });
        if let Some(e) = err {
            return Err(e);
        }
        if ctx.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        copy(&[], 0, copied, snap.len(), w, idx)?;
        Ok(count)
    }
}

/// What `Matcher::windows` reports: a match, or that the window it searched is done up to an offset.
enum Hit {
    Match(u64, u64),
    Done(u64),
}

/// Where to continue after match `m` (start, end; one character further after an empty match).
fn next_at(hay: &[u8], m: (usize, usize)) -> usize {
    if m.1 > m.0 { m.1 } else { m.1 + super::text::char_len_at(&hay[m.1..]).max(1) }
}

/// Matches counted (and their positions, up to a limit) by a background search.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub count: u64,
    pub positions: Vec<(u64, u64)>,
    pub complete: bool,
}

pub const MAX_POSITIONS: usize = 1_000_000;

pub fn count_all(m: &Matcher, snap: &Snapshot, ctx: &Ctx) -> Found {
    let mut found = Found::default();
    m.windows(snap, 0, snap.len(), Some(ctx), m.sizes(snap.len(), true), &mut |_, _, hit| {
        if let Hit::Match(s, e) = hit {
            found.count += 1;
            if found.positions.len() < MAX_POSITIONS {
                found.positions.push((s, e));
            }
        }
        true
    });
    found.complete = !ctx.cancelled();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::source::Source;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn q(text: &str) -> Query {
        Query { text: text.into(), ..Default::default() }
    }

    fn ctx() -> Ctx {
        Ctx { cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)) }
    }

    /// A snapshot made of many small pieces, so matches cross piece boundaries.
    fn fragmented(text: &[u8]) -> Snapshot {
        let mut b = Buffer::new();
        for chunk in text.chunks(7).rev() {
            b.insert(0, chunk);
        }
        b.snapshot()
    }

    #[test]
    fn plain_text_finds_what_the_regex_finds() {
        // every case of the letters, `ſ` and the Kelvin sign (the regex's case folding), keys and values as in JSON
        // (runs found first, then the query around them), across small windows and pieces
        let parts = [
            "status", "STATUS", "Status", "ſtatus", "ſtatuſ", "\u{212A}ey", "key", "KEY", "\"s\":", "é", "ß", " ", "\n", "x",
            "\"status\":\"", "\"ſtatus\":\"", "\"\u{212A}EY\":\"", "refunded\"", "REFUNDED\"", "\"", ":", "\"\"",
        ];
        let mut r = 0x5DEE_CE66_D1CE_4E5Bu64;
        let mut text = String::new();
        for _ in 0..20_000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            text.push_str(parts[(r % parts.len() as u64) as usize]);
        }
        let snap = fragmented(text.as_bytes());
        let mut anchored = 0;
        for query in [
            "status", "Status", "key", "\"s\":", "s", "k", "ss", "x x", "tus\nk", "é", "\"status\":\"refunded\"",
            "\"key\":\"REFUNDED", "\":\"refunded", "\"\"status", " key", "x\"status\":\"",
        ] {
            for match_case in [false, true] {
                let qq = Query { text: query.into(), match_case, ..Default::default() };
                let lit = Matcher::new(&qq).unwrap();
                assert!(lit.lit.is_some() || !match_case, "{query:?}");
                anchored += matches!(lit.lit, Some(Literal::Anchored { .. })) as u32;
                let mut re = lit.clone();
                re.lit = None;
                let (a, b) = (count_all(&lit, &snap, &ctx()), count_all(&re, &snap, &ctx()));
                assert!(a.count > 0 || match_case || query == "x x", "{query:?}");
                assert_eq!(a.positions, b.positions, "{query:?}, match case {match_case}");
                for at in (0..snap.len()).step_by(997) {
                    assert_eq!(lit.find_fwd(&snap, at, snap.len(), None), re.find_fwd(&snap, at, snap.len(), None), "{query:?} at {at}");
                    assert_eq!(lit.find_back(&snap, 0, at, None), re.find_back(&snap, 0, at, None), "{query:?} at {at}");
                }
                let hay = &text.as_bytes()[..5000];
                assert_eq!(lit.matches_in(hay, 7, 100), re.matches_in(hay, 7, 100));
            }
        }
        assert!(anchored >= 5, "{anchored}");
        // Whole word and regexes stay with the regex
        assert!(Matcher::new(&Query { text: "status".into(), whole_word: true, ..Default::default() }).unwrap().lit.is_none());
        assert!(Matcher::new(&Query { text: "s+".into(), regex: true, ..Default::default() }).unwrap().lit.is_none());
    }

    #[test]
    fn finds_across_windows_and_pieces() {
        let mut text = Vec::new();
        for i in 0..50_000 {
            text.extend_from_slice(format!("row {i} needle{} ", i % 3).as_bytes());
            if i % 10 == 0 {
                text.push(b'\n');
            }
        }
        let snap = fragmented(&text);
        let m = Matcher::new(&q("needle1")).unwrap();
        let naive = memchr::memmem::find_iter(&text, b"needle1").count() as u64;
        // tiny windows to exercise the window logic
        let mut n = 0;
        m.each(&snap, 0, snap.len(), None, 1000, &mut |_, _, s, e| {
            assert_eq!(&text[s as usize..e as usize], b"needle1");
            n += 1;
            true
        });
        assert_eq!(n, naive);
        let f = count_all(&m, &snap, &ctx());
        assert_eq!(f.count, naive);
        let first = memchr::memmem::find(&text, b"needle1").unwrap() as u64;
        assert_eq!(m.find_fwd(&snap, 0, snap.len(), None), Some((first, first + 7)));
        let last = memchr::memmem::rfind(&text, b"needle1").unwrap() as u64;
        assert_eq!(m.find_back(&snap, 0, snap.len(), None), Some((last, last + 7)));
        assert_eq!(m.find_back(&snap, 0, first + 6, None), None);
    }

    #[test]
    fn find_previous_and_the_current_match_see_the_text_around_them() {
        let snap = fragmented(b"foobar foo\nabc def\nabc\n");
        let word = Matcher::new(&Query { whole_word: true, ..q("foo") }).unwrap();
        // the caret between "foo" and "bar": "foo" isn't a whole word there
        assert_eq!(word.find_back(&snap, 0, 3, None), None);
        assert_eq!(word.find_back(&snap, 0, snap.len(), None), Some((7, 10)));
        let end = Matcher::new(&Query { regex: true, ..q("abc$") }).unwrap();
        // the caret after the first "abc", which isn't at a line end
        assert_eq!(end.find_back(&snap, 0, 14, None), None);
        assert_eq!(end.find_back(&snap, 0, snap.len(), None), Some((19, 22)));
        // is the selection a match where it is?
        assert!(!word.is_match_at(&snap, 0, 3));
        assert!(word.is_match_at(&snap, 7, 10));
        let run = Matcher::new(&Query { regex: true, ..q("o+") }).unwrap();
        assert!(!run.is_match_at(&snap, 1, 2)); // part of "oo"
        assert!(run.is_match_at(&snap, 1, 3));
        let empty = Matcher::new(&Query { regex: true, ..q("^") }).unwrap();
        assert!(empty.is_match_at(&snap, 11, 11));
        assert!(!empty.is_match_at(&snap, 12, 12));
    }

    #[test]
    fn options() {
        let snap = fragmented(b"Foo foo food FOO_bar foo.");
        let n = |query: Query| {
            let m = Matcher::new(&query).unwrap();
            count_all(&m, &snap, &ctx()).count
        };
        assert_eq!(n(q("foo")), 5);
        assert_eq!(n(Query { match_case: true, ..q("foo") }), 3);
        assert_eq!(n(Query { whole_word: true, ..q("foo") }), 3);
        assert_eq!(n(Query { regex: true, ..q(r"f\w+") }), 5);
        assert_eq!(n(Query { regex: true, whole_word: true, ..q("fo+") }), 3);
        assert!(Matcher::new(&Query { regex: true, ..q("(") }).is_err());
    }

    /// The matches of a search of the whole text in one piece, with `each`'s rules for empty matches.
    fn whole(m: &Matcher, text: &[u8]) -> Vec<(u64, u64)> {
        let (mut v, mut at, mut last_end) = (Vec::new(), 0, None);
        while at <= text.len() {
            let Some(x) = m.re.find_at(text, at) else { break };
            if !(x.start() == x.end() && last_end == Some(x.start())) {
                v.push((x.start() as u64, x.end() as u64));
            }
            last_end = Some(x.end());
            at = next_at(text, (x.start(), x.end()));
        }
        v
    }

    #[test]
    fn windows_find_what_the_whole_text_has() {
        // seams everywhere (tiny windows, and a tiny overlap in tests): the same matches as one search of it all
        let words = ["foo", "bar", "Foo", "é", "É", "\r\n", "\n", " ", "x", "aaaa", "\t", "foobar", "1.5", "日本", "\r"];
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        let mut text = Vec::new();
        for _ in 0..4000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            text.extend_from_slice(words[(r % words.len() as u64) as usize].as_bytes());
        }
        let snap = fragmented(&text);
        let pats = [
            "foo", r"\bfoo\b", "^", "$", r"^\s*$", "x*", r"\b", "a+", "é", "o$", r"\w+$", r"^\w", "é|É", r"\s+", "(?s).",
            r"foo\s+bar", "[^a]{20,}", "foob|o", r"(?-u:\B)", r"(?-u:\b)", "ab+a|b",
        ];
        for pat in pats {
            for case in [true, false] {
                let m = Matcher::new(&Query { regex: true, match_case: case, ..q(pat) }).unwrap();
                let want = whole(&m, &text);
                for window in [1, 5, 64, 1000] {
                    let mut got = Vec::new();
                    m.each(&snap, 0, snap.len(), None, window, &mut |_, _, s, e| {
                        got.push((s, e));
                        true
                    });
                    assert_eq!(got.len(), want.len(), "{pat} (case {case}), window {window}");
                    assert_eq!(got, want, "{pat} (case {case}), window {window}");
                }
            }
        }
    }

    /// The text with every match in `found` replaced, as Replace all would write it.
    fn replaced(m: &Matcher, text: &[u8], found: &[(u64, u64)], repl: &[u8]) -> Vec<u8> {
        let (mut out, mut at) = (Vec::new(), 0);
        for &(s, e) in found {
            out.extend_from_slice(&text[at..s as usize]);
            m.expand(text, s as usize, repl, &mut out);
            at = e as usize;
        }
        out.extend_from_slice(&text[at..]);
        out
    }

    #[test]
    fn long_matches_at_seams_and_the_ones_after_them() {
        // Strings longer than Find next's overlap (256 in tests): a match cut off by a window's end isn't a match at
        // all there, and a search going on from inside it finds the wrong ones after it (`", "`).
        let mut text = Vec::new();
        for i in 0..60 {
            text.extend_from_slice(format!("{{\"k\": \"{}\", \"n\": {i}}},\n", "x".repeat(i % 5 * 300)).as_bytes());
        }
        assert!(text.len() as u64 > ONE_WINDOW);
        let snap = fragmented(&text);
        for pat in [r#""[^"]*""#, r#"\{[^}]*\}"#, r#"(?s)"k".*?\}"#] {
            let m = Matcher::new(&Query { regex: true, ..q(pat) }).unwrap();
            let want = whole(&m, &text);
            // Count all and Replace all: exact while no match is longer than their overlap (1500 in tests)
            assert_eq!(count_all(&m, &snap, &ctx()).positions, want, "{pat}");
            let mut out = Vec::new();
            let n = m.replace_all_to(&snap, b"\"\"", &mut out, &mut IndexBuilder::new(), &ctx()).unwrap();
            assert_eq!(n, want.len() as u64);
            assert!(out == replaced(&m, &text, &want, b"\"\""), "{pat}");
        }
        // A document up to `ONE_WINDOW` is searched in one piece: a match of any length, and Find previous too.
        let mut small = b"{\"a\": \"".to_vec();
        small.extend(std::iter::repeat_n(b'x', 3000));
        small.extend_from_slice(b"\", \"b\": \"y\", \"c\": \"z\"}");
        assert!(small.len() as u64 <= ONE_WINDOW);
        let snap = fragmented(&small);
        let m = Matcher::new(&Query { regex: true, ..q(r#""[^"]*""#) }).unwrap();
        let want = whole(&m, &small);
        assert_eq!(count_all(&m, &snap, &ctx()).positions, want);
        assert_eq!(m.find_fwd(&snap, 0, snap.len(), None), want.first().copied());
        for caret in (0..=small.len() as u64).step_by(97).chain([small.len() as u64]) {
            let before = want.iter().copied().filter(|&(_, e)| e <= caret).last();
            assert_eq!(m.find_back(&snap, 0, caret, None), before, "previous before {caret}");
        }
    }

    #[test]
    fn a_short_match_that_runs_past_the_overlap_is_found_from_its_start() {
        // "ab…a" starting in a window's overlap and ending past it: the window can't see it whole, but a "b" inside
        // it matches; the next window must search from where this window's part ends, not from that "b".
        let m = Matcher::new(&Query { regex: true, ..q("ab+a|b") }).unwrap();
        for at in 990..1020 {
            let mut text = vec![b'.'; at];
            text.extend_from_slice(&[b'a', b'b', b'b', b'b', b'b', b'b', b'a']);
            text.extend(std::iter::repeat_n(b'.', 600));
            let snap = fragmented(&text);
            let mut got = Vec::new();
            // a window of 1000 with an overlap of 16 (the match is 7 long): in the overlap at 1000..1016
            m.windows(&snap, 0, snap.len(), None, (1000, 16), &mut |_, _, hit| {
                if let Hit::Match(s, e) = hit {
                    got.push((s, e));
                }
                true
            });
            assert_eq!(got, whole(&m, &text), "starting at {at}");
        }
    }

    #[test]
    fn replace_all_across_windows_is_one_replace_of_the_whole_text() {
        // (the text between matches is written from the window it was searched in)
        let mut text = Vec::new();
        for i in 0..2000 {
            text.extend_from_slice(format!("id={i} name=\"n{}\" {}\r\n", i % 13, "pad ".repeat(i % 5)).as_bytes());
        }
        let snap = fragmented(&text);
        for (pat, regex, repl) in [
            ("id=", false, "ID:"),
            ("pad", false, ""),
            (r"(\w+)=(\d+)", true, "$2<-$1"),
            (r#""[^"]*""#, true, "'q'"),
            ("^", true, "> "),
            (r"\s+$", true, ""),
            ("x*", true, "-"),
        ] {
            let m = Matcher::new(&Query { regex, match_case: true, ..q(pat) }).unwrap();
            let mut want = Vec::new();
            let mut at = 0;
            let found = whole(&m, &text);
            for &(s, e) in &found {
                want.extend_from_slice(&text[at..s as usize]);
                m.expand(&text, s as usize, repl.as_bytes(), &mut want);
                at = e as usize;
            }
            want.extend_from_slice(&text[at..]);
            let mut out = Vec::new();
            let mut idx = IndexBuilder::new();
            let n = m.replace_all_to(&snap, repl.as_bytes(), &mut out, &mut idx, &ctx()).unwrap();
            assert_eq!(n, found.len() as u64, "{pat}");
            assert!(out == want, "{pat}");
            assert_eq!(idx.newlines(), bytecount::count(&want, b'\n') as u64);
        }
    }

    #[test]
    fn long_matches_grow_the_window() {
        let mut text = b"begin ".to_vec();
        text.extend(std::iter::repeat_n(b'a', 300_000));
        text.extend_from_slice(b" end");
        let snap = fragmented(&text);
        let m = Matcher::new(&Query { regex: true, ..q("a+") }).unwrap();
        let mut got = Vec::new();
        m.each(&snap, 0, snap.len(), None, 1000, &mut |_, _, s, e| {
            got.push((s, e));
            true
        });
        assert_eq!(got, vec![(6, 300_006)]);
    }

    #[test]
    fn replace_all_with_groups() {
        let text = b"a=1, b=22\nc=333";
        let snap = fragmented(text);
        let m = Matcher::new(&Query { regex: true, ..q(r"(\w)=(\d+)") }).unwrap();
        let mut out = Vec::new();
        let mut idx = IndexBuilder::new();
        let n = m.replace_all_to(&snap, b"$2:$1", &mut out, &mut idx, &ctx()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out, b"1:a, 22:b\n333:c");
        assert_eq!(idx.newlines(), 1);
        // Empty matches: insert at every line start.
        let m = Matcher::new(&Query { regex: true, ..q("^") }).unwrap();
        let snap = Arc::new(Source::from_vec(b"x\ny\n".to_vec()));
        let mut b = Buffer::from_source(snap, 2);
        let snap = b.snapshot();
        let mut out = Vec::new();
        m.replace_all_to(&snap, b"> ", &mut out, &mut IndexBuilder::new(), &ctx()).unwrap();
        assert_eq!(out, b"> x\n> y\n> ");
    }

    /// Times ways of searching plain text ignoring case over a file (`SLATE_SEARCH_FILE`):
    /// `SLATE_SEARCH_FILE=<file> cargo test --release --lib search_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn search_speed() {
        let Some(p) = std::env::var_os("SLATE_SEARCH_FILE") else { return };
        let text = std::fs::read(p).unwrap();
        let time = |name: &str, f: &dyn Fn() -> usize| {
            let t = std::time::Instant::now();
            let n = f();
            println!("{name:40} {n:>9} in {:7.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        };
        for query in ["\"status\":\"refunded\"", "Leave at the door", "refunded", "Atlanta"] {
            println!("-- {query}");
            let re = RegexBuilder::new(&regex::escape(query)).case_insensitive(true).build().unwrap();
            time("regex", &|| re.find_iter(&text).count());
            let m = Matcher::new(&Query { text: query.into(), ..Default::default() }).unwrap();
            time(if m.lit.is_some() { "Matcher (anchored)" } else { "Matcher (regex)" }, &|| {
                let (mut n, mut at) = (0, 0);
                while let Some((_, e)) = m.find_at(&text, at) {
                    n += 1;
                    at = e;
                }
                n
            });
            let src = Arc::new(Source::from_vec(text.clone()));
            let mut b = Buffer::from_source(src, 0);
            let snap = b.snapshot();
            time("count_all", &|| count_all(&m, &snap, &ctx()).count as usize);
        }
    }
}
