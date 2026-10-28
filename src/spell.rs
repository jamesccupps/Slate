//! Spell check (both apps): which words of the text on screen are worth checking, and what's known of each. The
//! checking is the system's: Windows' own spell checker (`ui::spell`), Enchant on Linux (`gtk::spell`). Only the
//! words on screen are ever looked at, so a file's size doesn't matter, and each word is asked about once.

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

#[cfg(test)]
mod tests {
    use super::*;

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
