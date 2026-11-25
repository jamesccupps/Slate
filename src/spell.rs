//! Spell check (both apps): which words of the text are worth checking, and what's known of each. The checking is
//! the system's: Windows' own spell checker (`ui::spell`), Enchant on Linux (`gtk::spell`). The marks are for the
//! words on screen only, so a file's size doesn't matter to them; F7 (`next_wrong`) and Change every (`places_of`)
//! look further, a part at a time. Each word is asked about once.

use std::collections::HashMap;

use crate::highlight::{self, Lang, Span, State, Tok};

/// A system's spell checker.
pub trait Checker {
    /// Spelled right (or not something it can judge).
    fn check(&self, word: &str) -> bool;
    /// What it may have been meant to be, likeliest first.
    fn suggest(&self, word: &str) -> Vec<String>;
    /// Into the user's own dictionary, for good (other programs use it too).
    fn add(&self, word: &str);
    /// Right from now on, until Slate ends.
    fn ignore(&self, word: &str);
}

/// At most this many suggestions are offered.
pub const SUGGEST_MAX: usize = 5;

/// Words checked are remembered, up to this many (then it starts over).
const KNOWN_MAX: usize = 100_000;

pub struct Speller {
    checker: Box<dyn Checker>,
    /// Each word asked about, and whether it's right.
    known: HashMap<String, bool>,
    /// What the dictionary's language is called (`en-US`).
    pub lang: String,
}

impl Speller {
    pub fn new(checker: Box<dyn Checker>, lang: String) -> Speller {
        Speller { checker, known: HashMap::new(), lang }
    }

    pub fn wrong(&mut self, word: &str) -> bool {
        if let Some(&ok) = self.known.get(word) {
            return !ok;
        }
        if self.known.len() >= KNOWN_MAX {
            self.known.clear();
        }
        let ok = self.checker.check(word);
        self.known.insert(word.to_string(), ok);
        !ok
    }

    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut v = self.checker.suggest(word);
        v.truncate(SUGGEST_MAX);
        v
    }

    /// Add to dictionary (`keep`), or Ignore.
    pub fn accept(&mut self, word: &str, keep: bool) {
        if keep {
            self.checker.add(word);
        } else {
            self.checker.ignore(word);
        }
        self.known.insert(word.to_string(), true);
    }
}

/// Whether anything in a document in `lang` is checked (`words_in`).
pub fn checks(lang: Lang) -> bool {
    matches!(lang, Lang::Plain | Lang::Markdown) || lang.comment().is_some()
}

/// Where the words worth checking are in `text`, a segment of a document in `lang` whose lexing starts in `st` (byte
/// ranges in `text`): in plain text all of them; in Markdown its prose, not code or links; in code and settings files
/// only comments; in data (JSON, CSV, logs…) none. `spans`: room for the lexer's.
pub fn words_in(lang: Lang, text: &[u8], st: State, spans: &mut Vec<Span>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    match lang {
        Lang::Plain => words(text, 0, text.len(), &mut out),
        Lang::Markdown => {
            if highlight::markdown_code(st) {
                return out;
            }
            highlight::lex(lang, text, st, Some(spans));
            // (prose: text with no color, headings, bold, italic, quotes; not code, links or HTML)
            let mut at = 0;
            for &(s, e, tok) in spans.iter() {
                let (s, e) = (s as usize, e as usize);
                if matches!(tok, Tok::Heading | Tok::Bold | Tok::Italic | Tok::Comment) {
                    continue;
                }
                words(text, at, s.max(at), &mut out);
                at = at.max(e);
            }
            words(text, at, text.len(), &mut out);
        }
        _ if lang.comment().is_some() => {
            highlight::lex(lang, text, st, Some(spans));
            for &(s, e, tok) in spans.iter() {
                if tok == Tok::Comment {
                    words(text, s as usize, e as usize, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

/// The words in `text[a..b]`: letters, with an apostrophe between two of them (`don't`). Not ones that are names or
/// part of something else: with a digit, an underscore or a slash next to them, after a dot or before one followed
/// by a letter (`file.txt`, `example.com`), in an e-mail address or a link, with a capital letter after the first
/// (`camelCase`, `NASA`), of one letter, or in scripts written without spaces between words or not checked by
/// these dictionaries (from Hebrew on: Arabic, Indic, Thai, Chinese, Japanese, Korean).
fn words(text: &[u8], a: usize, b: usize, out: &mut Vec<(usize, usize)>) {
    if a >= b {
        return;
    }
    let before = |i: usize| -> Option<char> { char_before(text, i) };
    let after = |i: usize| -> Option<char> { char_at(text, i) };
    let mut off = a;
    for chunk in text[a..b].utf8_chunks() {
        let s = chunk.valid();
        let mut it = s.char_indices().peekable();
        while let Some((i, c)) = it.next() {
            if !c.is_alphabetic() {
                continue;
            }
            let start = i;
            let mut end = i + c.len_utf8();
            let (mut capital_inside, mut odd) = (false, c as u32 >= 0x590);
            let mut letters = 1;
            while let Some(&(j, d)) = it.peek() {
                if d.is_alphabetic() {
                    capital_inside |= d.is_uppercase();
                    odd |= d as u32 >= 0x590;
                    letters += 1;
                    end = j + d.len_utf8();
                    it.next();
                } else if matches!(d, '\'' | '\u{2019}') && s[j + d.len_utf8()..].chars().next().is_some_and(char::is_alphabetic) {
                    end = j + d.len_utf8();
                    it.next();
                } else {
                    break;
                }
            }
            let (ws, we) = (off + start, off + end);
            let joined = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || matches!(c, '_' | '/' | '\\' | '@'));
            let prev = before(ws);
            let next = after(we);
            let dotted = prev == Some('.') || (next == Some('.') && char_at(text, we + 1).is_some_and(char::is_alphanumeric));
            let link = next == Some(':') && text.get(we + 1..we + 3) == Some(b"//");
            let named = prev.is_some_and(|c| matches!(c, '#' | '$' | '%' | '&' | '='));
            if letters >= 2 && !capital_inside && !odd && !joined(prev) && !joined(next) && !dotted && !link && !named {
                out.push((ws, we));
            }
        }
        off += chunk.valid().len() + chunk.invalid().len();
    }
}

fn char_at(text: &[u8], i: usize) -> Option<char> {
    let rest = text.get(i..)?;
    let n = rest.len().min(4);
    (1..=n).find_map(|k| std::str::from_utf8(&rest[..k]).ok()).and_then(|s| s.chars().next())
}

fn char_before(text: &[u8], i: usize) -> Option<char> {
    let head = text.get(..i)?;
    (1..=head.len().min(4)).find_map(|k| std::str::from_utf8(&head[head.len() - k..]).ok()).and_then(|s| s.chars().next())
}

/// Where the next word spelled wrong is (F7, issue #12).
#[derive(Debug, PartialEq, Eq)]
pub enum NextWrong {
    /// Its byte range in the document.
    Found(u64, u64),
    /// None up to the end of the range looked at.
    Clean,
    /// None in the bytes it may look at (`budget`), which end here.
    Stopped(u64),
}

/// The first word spelled wrong that starts at `from` or after it and ends by `to`, looking at `budget` bytes at
/// most, and until `until` (a text of words new to the checker takes a while: the window mustn't wait for seconds).
/// `words(pos)` gives the words worth checking in the segment at `pos` (`words_in`'s, as document ranges, with the
/// word) and where the next segment starts.
pub fn next_wrong(
    sp: &mut Speller,
    from: u64,
    to: u64,
    budget: u64,
    until: std::time::Instant,
    mut words: impl FnMut(u64) -> (u64, Vec<(u64, u64, String)>),
) -> NextWrong {
    let mut pos = from;
    while pos < to {
        if pos - from >= budget || (pos > from && std::time::Instant::now() >= until) {
            return NextWrong::Stopped(pos);
        }
        let (next, found) = words(pos);
        for (a, b, w) in found {
            if a >= from && b <= to && sp.wrong(&w) {
                return NextWrong::Found(a, b);
            }
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    NextWrong::Clean
}

/// How long F7 may look before it stops ("F7 looks on from here"), and the most it reads.
pub const F7_TIME: std::time::Duration = std::time::Duration::from_millis(150);
pub const F7_BYTES: u64 = 8 << 20;
/// The biggest text Change every looks through for the spell check's places (`places_of`); a bigger one gets a
/// whole-word Replace all.
pub const CHANGE_EVERY_MAX: u64 = 8 << 20;

/// Where `word` is one of the words worth checking (`words`, as for `next_wrong`) in the text up to `len`: Change
/// every's places, so the same letters in code, a file name or a link (which the spell check passes over) stay.
pub fn places_of(word: &str, len: u64, mut words: impl FnMut(u64) -> (u64, Vec<(u64, u64, String)>)) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut pos = 0;
    while pos < len {
        let (next, found) = words(pos);
        for (a, b, w) in found {
            if a >= pos && w == word && out.last().is_none_or(|&(_, e)| e <= a) {
                out.push((a, b));
            }
        }
        if next <= pos {
            break;
        }
        pos = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words starting with "zz" are wrong.
    struct Zz;

    impl Checker for Zz {
        fn check(&self, w: &str) -> bool {
            !w.starts_with("zz")
        }
        fn suggest(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn add(&self, _: &str) {}
        fn ignore(&self, _: &str) {}
    }

    #[test]
    fn the_next_word_spelled_wrong() {
        let text = "one zztwo\nthree\nfour zzfive six";
        let mut sp = Speller::new(Box::new(Zz), "xx".into());
        // (each line a segment)
        let words = |pos: u64| {
            let start = text[..pos as usize].rfind('\n').map_or(0, |i| i + 1);
            let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
            let mut spans = Vec::new();
            let ws = words_in(Lang::Plain, &text.as_bytes()[start..end], State::START, &mut spans)
                .into_iter()
                .map(|(a, b)| ((start + a) as u64, (start + b) as u64, text[start + a..start + b].to_string()))
                .collect();
            ((end + 1) as u64, ws)
        };
        let len = text.len() as u64;
        let far = std::time::Instant::now() + std::time::Duration::from_secs(60);
        assert_eq!(next_wrong(&mut sp, 0, len, 1 << 20, far, words), NextWrong::Found(4, 9));
        // from the end of that one (F7 again): the next one
        assert_eq!(next_wrong(&mut sp, 9, len, 1 << 20, far, words), NextWrong::Found(21, 27));
        assert_eq!(next_wrong(&mut sp, 27, len, 1 << 20, far, words), NextWrong::Clean);
        // only up to `to` (round from the start to where it began)
        assert_eq!(next_wrong(&mut sp, 0, 20, 1 << 20, far, words), NextWrong::Found(4, 9));
        assert_eq!(next_wrong(&mut sp, 10, 26, 1 << 20, far, words), NextWrong::Clean);
        // allowed to look only so far: stops there
        assert_eq!(next_wrong(&mut sp, 10, len, 4, far, words), NextWrong::Stopped(16));
        // (out of time: after the first segment, wherever it is)
        let past = std::time::Instant::now();
        assert_eq!(next_wrong(&mut sp, 10, len, 1 << 20, past, words), NextWrong::Stopped(16));
    }

    #[test]
    fn change_every_only_where_the_spell_check_looks() {
        // (code, a file name and a link keep the same letters)
        let text = "teh one\n`teh` two\nsee teh.txt or http://teh.example.com\nteh end";
        let words = |pos: u64| {
            let start = text[..pos as usize].rfind('\n').map_or(0, |i| i + 1);
            let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
            let mut spans = Vec::new();
            let ws = words_in(Lang::Markdown, &text.as_bytes()[start..end], State::START, &mut spans)
                .into_iter()
                .map(|(a, b)| ((start + a) as u64, (start + b) as u64, text[start + a..start + b].to_string()))
                .collect();
            ((end + 1) as u64, ws)
        };
        let places = places_of("teh", text.len() as u64, words);
        let last = text.rfind("teh").unwrap() as u64;
        assert_eq!(places, vec![(0, 3), (last, last + 3)]);
        // (same case only: "Teh" is another word)
        assert!(places_of("Teh", text.len() as u64, words).is_empty());
    }

    fn found(lang: Lang, text: &str) -> Vec<&str> {
        let mut spans = Vec::new();
        words_in(lang, text.as_bytes(), State::START, &mut spans).into_iter().map(|(a, b)| &text[a..b]).collect()
    }

    #[test]
    fn the_words_worth_checking() {
        assert_eq!(found(Lang::Plain, "Teh cat don't sit."), vec!["Teh", "cat", "don't", "sit"]);
        assert_eq!(found(Lang::Plain, "a well-known café’s menu"), vec!["well", "known", "café’s", "menu"]);
        // names, links, files, addresses, numbers, acronyms, one letter
        let none = "camelCase snake_case NASA x2 3rd see.this.org file.txt https://exampel.com/pagee me@mial.com C:\\Windoes #taag $vaar a";
        assert!(found(Lang::Plain, none).is_empty());
        // a sentence's last word is checked; scripts without spaces aren't
        assert_eq!(found(Lang::Plain, "The end. 日本語 Привет"), vec!["The", "end", "Привет"]);
        // bytes that aren't UTF-8 split words
        assert_eq!(words_in(Lang::Plain, b"ab\xFFcd", State::START, &mut Vec::new()), vec![(0, 2), (3, 5)]);
        // code: comments only; Markdown: prose, not code or links
        assert_eq!(found(Lang::Rust, "let wrod = 1; // a coment here"), vec!["coment", "here"]);
        assert_eq!(found(Lang::Markdown, "# Titel here"), vec!["Titel", "here"]);
        assert_eq!(found(Lang::Markdown, "Some `codde` and [lnk](http://x.org/pth) **bold**"), vec!["Some", "and", "bold"]);
        assert_eq!(found(Lang::Json, "{\"wrod\": \"vaule\"}"), Vec::<&str>::new());
        // inside a Markdown code block, nothing
        let mut st = State::START;
        for line in ["```\n", "some codde\n"] {
            let w = words_in(Lang::Markdown, line.as_bytes(), st, &mut Vec::new());
            if line.starts_with("some") {
                assert!(w.is_empty());
            }
            st = highlight::lex(Lang::Markdown, line.as_bytes(), st, None);
        }
    }

    struct Fake;
    impl Checker for Fake {
        fn check(&self, w: &str) -> bool {
            w != "teh"
        }
        fn suggest(&self, _: &str) -> Vec<String> {
            (0..9).map(|i| format!("s{i}")).collect()
        }
        fn add(&self, _: &str) {}
        fn ignore(&self, _: &str) {}
    }

    #[test]
    fn words_are_asked_about_once_and_accepted() {
        let mut s = Speller::new(Box::new(Fake), "xx".into());
        assert!(s.wrong("teh") && !s.wrong("the") && s.wrong("teh"));
        assert_eq!(s.suggest("teh").len(), SUGGEST_MAX);
        s.accept("teh", false);
        assert!(!s.wrong("teh"));
    }
}
