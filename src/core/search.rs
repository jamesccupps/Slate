//! Find and replace over documents of any size. The text is searched in windows of a few MB (zero-copy when a
//! window sits in one in-memory piece), so even multi-GB files stream through at disk speed. Every query becomes
//! a byte regex (plain text is escaped), which also gives fast literal search.
//!
//! A match that starts in a window is looked for in it and the overlap after it: one that runs into the end of
//! that is looked for again in a bigger window, but one that ends further on can't be told from no match there (a
//! regex like `"[^"]*"` over a long string). So regexes get a bigger overlap: their matches up to 1 MiB long are
//! found exactly as in one search of the whole text; longer ones may be cut short or missed at a seam.

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
/// Extra bytes after a window so matches that start inside it can finish (plain text: as long as the query; small
/// in tests, so they cross many seams).
const OVERLAP: u64 = if cfg!(test) { 64 } else { 64 * 1024 };
/// The same for regexes, whose matches can be of any length (see the module docs).
const OVERLAP_REGEX: u64 = if cfg!(test) { 1024 } else { 1 << 20 };
/// The window searched at a time (small in tests, so they cross many seams).
const WINDOW: u64 = if cfg!(test) { 1000 } else { 8 << 20 };
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
    /// Bytes after a window a match that starts in it may need (see `OVERLAP`).
    overlap: u64,
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
        Ok(Matcher { re, regex_mode: q.regex, overlap })
    }

    /// First match starting in `[from, to)`.
    pub fn find_fwd(&self, h: &dyn Haystack, from: u64, to: u64, ctx: Option<&Ctx>) -> Option<(u64, u64)> {
        let mut found = None;
        self.each(h, from, to, ctx, WINDOW, &mut |_, _, s, e| {
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
            let he = (end + self.overlap).min(len);
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
        self.windows(h, from, to, ctx, window, &mut |hay, hs, hit| match hit {
            Hit::Match(s, e) => f(hay, hs, s, e),
            Hit::Done(_) => true,
        });
    }

    /// `each`, also telling `f` when a window is done: no match starts before `Hit::Done`'s offset that wasn't
    /// reported (so the text up to there can be taken from that window's `hay` while it's there).
    fn windows(
        &self,
        h: &dyn Haystack,
        from: u64,
        to: u64,
        ctx: Option<&Ctx>,
        window: u64,
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
            let he = (pos + win + self.overlap).min(len);
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
                        if !f(hay, hs, Hit::Match(ms, me)) {
                            return;
                        }
                        next = me.max(ms + 1);
                    }
                    break;
                }
                // Skip an empty match right where the previous match ended.
                if !(m.start() == m.end() && last_end == Some(ms)) && !f(hay, hs, Hit::Match(ms, me)) {
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
            if !f(hay, hs, Hit::Done(next)) {
                return;
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
        self.windows(snap, 0, snap.len(), Some(ctx), WINDOW, &mut |hay, hs, hit| {
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
    m.each(snap, 0, snap.len(), Some(ctx), WINDOW, &mut |_, _, s, e| {
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

    /// The matches of a search of the whole text in one piece, with `each`'s rules for empty matches.
    fn whole(m: &Matcher, text: &[u8]) -> Vec<(u64, u64)> {
        let (mut v, mut at, mut last_end) = (Vec::new(), 0, None);
        while at <= text.len() {
            let Some(x) = m.re.find_at(text, at) else { break };
            if !(x.start() == x.end() && last_end == Some(x.start())) {
                v.push((x.start() as u64, x.end() as u64));
            }
            last_end = Some(x.end());
            at = next_at(text, &x);
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
            r"foo\s+bar", "[^a]{20,}",
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

    #[test]
    fn long_matches_across_seams_are_found_whole() {
        // strings longer than a literal search's overlap: a match cut off by a window's end isn't a match at all
        // there, and a search that went on from that window's end would find the wrong ones (`", "`)
        let mut text = Vec::new();
        for i in 0..300 {
            text.extend_from_slice(format!("{{\"k\": \"{}\", \"n\": {i}}}\n", "x".repeat(i % 7 * 50)).as_bytes());
        }
        let snap = fragmented(&text);
        for pat in [r#""[^"]*""#, r#"\{[^}]*\}"#, r#"(?s)"k".*?\}"#] {
            let m = Matcher::new(&Query { regex: true, ..q(pat) }).unwrap();
            let want = whole(&m, &text);
            for window in [1, 100, 1000] {
                let mut got = Vec::new();
                m.each(&snap, 0, snap.len(), None, window, &mut |_, _, s, e| {
                    got.push((s, e));
                    true
                });
                assert_eq!(got, want, "{pat}, window {window}");
            }
            let found = count_all(&m, &snap, &ctx());
            assert_eq!(found.positions, want);
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
}
