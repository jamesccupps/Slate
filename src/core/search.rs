//! Find and replace over documents of any size. The text is searched in windows of a few MB (zero-copy when a
//! window sits in one in-memory piece), so even multi-GB files stream through at disk speed. Every query becomes
//! a byte regex (plain text is escaped), which also gives fast literal search.

use std::io::{self, Write};

use regex::bytes::{Match, Regex, RegexBuilder};

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
/// Extra bytes after a window so matches that start inside it can finish.
const OVERLAP: u64 = 64 * 1024;
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

pub struct Matcher {
    re: Regex,
    regex_mode: bool,
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
        Ok(Matcher { re, regex_mode: q.regex })
    }

    /// First match starting in `[from, to)`.
    pub fn find_fwd(&self, h: &dyn Haystack, from: u64, to: u64, ctx: Option<&Ctx>) -> Option<(u64, u64)> {
        let mut found = None;
        self.each(h, from, to, ctx, 8 << 20, &mut |_, _, s, e| {
            found = Some((s, e));
            false
        });
        found
    }

    /// Last match that starts at or after `from` and ends at or before `to`. The text after `to` still counts for
    /// whether something is a match (`\b`, `$`): the whole word "foo" isn't in "foobar", wherever the caret is.
    pub fn find_back(&self, h: &dyn Haystack, from: u64, to: u64, ctx: Option<&Ctx>) -> Option<(u64, u64)> {
        let len = h.hay_len();
        let to = to.min(len);
        let mut win = 1u64 << 20;
        let mut end = to;
        let mut scratch = Vec::new();
        while end > from {
            if ctx.is_some_and(|c| c.cancelled()) {
                return None;
            }
            let start = end.saturating_sub(win).max(from);
            let hs = start.saturating_sub(CTX);
            let he = (end + OVERLAP).min(len);
            let hay = h.hay(hs, he, &mut scratch);
            let mut best = None;
            let mut at = (start - hs) as usize;
            while at <= hay.len() {
                let Some(m) = self.re.find_at(hay, at) else { break };
                let ms = hs + m.start() as u64;
                if ms >= end {
                    break;
                }
                let me = hs + m.end() as u64;
                if me <= to {
                    best = Some((ms, me));
                }
                at = next_at(hay, &m);
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
            let he = (pos + win + OVERLAP).min(len);
            let limit = if he == len { u64::MAX } else { pos + win };
            let hay = h.hay(hs, he, &mut scratch);
            let mut at = (pos - hs) as usize;
            let mut next = limit.min(to);
            let mut grow = false;
            while at <= hay.len() {
                let Some(m) = self.re.find_at(hay, at) else { break };
                let ms = hs + m.start() as u64;
                let me = hs + m.end() as u64;
                // An empty match at the very end of the text counts (e.g. `^` after a final line break).
                if ms > to || (ms == to && (to < len || me > ms)) {
                    return;
                }
                if ms >= limit {
                    next = ms;
                    break;
                }
                if m.end() == hay.len() && he < len {
                    // The match may continue past this window: search again from it with a bigger window.
                    next = ms;
                    grow = win < MAX_WINDOW;
                    if !grow {
                        // Give up on extending; report what we have.
                        if !f(hay, hs, ms, me) {
                            return;
                        }
                        next = me.max(ms + 1);
                    }
                    break;
                }
                // Skip an empty match right where the previous match ended.
                if !(m.start() == m.end() && last_end == Some(ms)) && !f(hay, hs, ms, me) {
                    return;
                }
                last_end = Some(me);
                at = next_at(hay, &m);
                next = me.max(limit.min(to));
            }
            if grow {
                win = (win * 4).min(MAX_WINDOW);
            } else {
                win = window;
            }
            if next <= pos && !grow {
                next = pos + 1;
            }
            pos = next;
        }
    }

    /// All matches within `hay` (a small piece of text such as the visible lines), as offsets from `base`.
    pub fn matches_in(&self, hay: &[u8], base: u64, limit: usize) -> Vec<(u64, u64)> {
        self.re
            .find_iter(hay)
            .filter(|m| m.start() != m.end())
            .take(limit)
            .map(|m| (base + m.start() as u64, base + m.end() as u64))
            .collect()
    }

    /// Whether `[s, e)` of `h` is a match where it is, with the text around it (to decide if the selection is the
    /// current match): with `\b`, `^`, `$` or a run that goes on, a piece of text alone can match where it doesn't.
    pub fn is_match_at(&self, h: &dyn Haystack, s: u64, e: u64) -> bool {
        let hs = s.saturating_sub(CTX);
        let he = (e + CTX).min(h.hay_len());
        let mut scratch = Vec::new();
        let hay = h.hay(hs, he, &mut scratch);
        self.re.find_at(hay, (s - hs) as usize).is_some_and(|m| hs + m.start() as u64 == s && hs + m.end() as u64 == e)
    }

    /// The replacement for the match at `[s, e)` of `hay` (expands `$1`, `${name}` in regex mode).
    pub fn expand(&self, hay: &[u8], s: usize, replacement: &[u8], out: &mut Vec<u8>) {
        if !self.regex_mode {
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
        let copy = |a: u64, b: u64, w: &mut dyn Write, idx: &mut IndexBuilder| -> io::Result<()> {
            let mut e = None;
            snap.chunks(a, b, &mut |c| {
                idx.push(c);
                if let Err(x) = w.write_all(c) {
                    e = Some(x);
                    return false;
                }
                true
            });
            e.map_or(Ok(()), Err)
        };
        self.each(snap, 0, snap.len(), Some(ctx), 8 << 20, &mut |hay, hs, s, e| {
            if let Err(x) = copy(copied, s, w, idx) {
                err = Some(x);
                return false;
            }
            buf.clear();
            self.expand(hay, (s - hs) as usize, replacement, &mut buf);
            idx.push(&buf);
            if let Err(x) = w.write_all(&buf) {
                err = Some(x);
                return false;
            }
            copied = e;
            count += 1;
            true
        });
        if let Some(e) = err {
            return Err(e);
        }
        if ctx.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        copy(copied, snap.len(), w, idx)?;
        Ok(count)
    }
}

/// Where to continue after match `m` (one character further after an empty match).
fn next_at(hay: &[u8], m: &Match) -> usize {
    if m.end() > m.start() {
        m.end()
    } else {
        m.end() + super::text::char_len_at(&hay[m.end()..]).max(1)
    }
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
    m.each(snap, 0, snap.len(), Some(ctx), 8 << 20, &mut |_, _, s, e| {
        found.count += 1;
        if found.positions.len() < MAX_POSITIONS {
            found.positions.push((s, e));
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
}
