//! Hostile files (`cargo test --release --lib fuzz -- --ignored --nocapture`): random and mutated text through every
//! language's lexer (whole, and in pieces as coloring does: in three at line breaks it must end in the state lexing
//! it whole does), JSON and XML Check and Format, the structure panel's scans and paths, CSV Check and Sort by column,
//! the line tools, search (the input's own bytes as a plain and a regex query), encoding detection and the UTF-16 and
//! ANSI codecs (UTF-16 there and back must give the text back), how the text is drawn and stepped through, Convert's
//! decoders (Base64 and URL encoding there and back), EditorConfig's globs, settings.json and shortcuts.txt (every key
//! it takes written back reads the same), and multiple cursors (typing, deleting, Enter and moving at cursors put
//! anywhere: they stay in the text, in order and apart, for its version). A panic, or a property
//! that doesn't hold, is reported with its input. An input that keeps one step busy for 10 s (a loop
//! that doesn't end: a file that would freeze the window) stops the run, its bytes written to `slate-fuzz-hang.bin`
//! in the temp folder. `SLATE_FUZZ_ROUNDS` sets how many inputs (default 2000), `SLATE_FUZZ_SEED` where they start.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::csv::{self, Dialect};
use crate::core::document::Document;
use crate::core::job::Ctx;
use crate::core::json::{self, Mode};
use crate::core::jsonnav::{self, Children};
use crate::core::lines::{self, LineOp};
use crate::core::search::{self, Matcher, Query};
use crate::core::text::{self, AnsiDecoder, AnsiEncoder, Utf16Decoder, Utf16Encoder};
use crate::core::{xml, xmlnav};
use crate::highlight::{self, Lang, State};

/// What languages are made of: quotes, comments, brackets, escapes, markup, here-documents, bytes that aren't text.
const PIECES: &[&[u8]] = &[
    b"\"", b"'", b"`", b"\\", b"/*", b"*/", b"//", b"#", b"--", b"<!--", b"-->", b"<", b">", b"</", b"/>", b"<?", b"?>",
    b"<![CDATA[", b"]]>", b"<!DOCTYPE x [", b"{", b"}", b"[", b"]", b"(", b")", b"${", b"$", b"@\"", b"r#\"", b"\"#",
    b"\"\"\"", b"'''", b"<<EOF\n", b"EOF\n", b"@'", b"'@", b"\n", b"\r", b"\r\n", b"\t", b" ", b"=", b":", b",", b";",
    b"\0", b"\xff", b"\xe2\x82", b"\xf0\x9f\x98\x80", b"\xcc\x81", b"a", b"key", b"123", b"-1.5e+9", b"true", b"null",
    b"```", b"```rust\n", b"[Code]", b"C ", b"00010 ", b"GOTO", b"%X%", b"m/", b"s{", b"qw(", b"=pod\n", b"=cut\n",
    b"&lt;", b"&#x1F600;", b"<a b='c'>", b"</a>", b"{\"k\":", b"\\u12", b"\\\n", b"@media (", b"server {", b"[section]\n",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// An input: pieces, random bytes, now and then one piece many times over (deep nesting, a long run).
fn input(r: &mut Rng) -> Vec<u8> {
    let n = if r.below(50) == 0 { 2000 + r.below(20_000) } else { r.below(400) };
    let mut out = Vec::new();
    for _ in 0..n {
        match r.below(100) {
            0..=9 => out.extend((0..1 + r.below(8)).map(|_| r.next() as u8)),
            10..=11 => {
                let p = PIECES[r.below(PIECES.len() as u64) as usize];
                for _ in 0..1 + r.below(5000) {
                    out.extend_from_slice(p);
                }
            }
            _ => out.extend_from_slice(PIECES[r.below(PIECES.len() as u64) as usize]),
        }
    }
    out
}

/// The structure panel on `doc`: the path at `offset`, scanning what's missing as the panel does.
fn path(doc: &Document, offset: u64, is_xml: bool) {
    let mut cache: std::collections::HashMap<Option<u64>, Arc<Children>> = Default::default();
    for _ in 0..64 {
        let get = &mut |o: Option<u64>| cache.get(&o).cloned();
        let r = if is_xml { xmlnav::path_at(doc, offset, &[], get) } else { jsonnav::path_at(doc, offset, &[], get) };
        match r {
            Ok(_) => return,
            Err((open, end)) => {
                let lists = if is_xml {
                    xmlnav::scan(doc, open, Some(end), 1024, Some(offset), None)
                } else {
                    jsonnav::scan(doc, open, 1024, Some(offset), None)
                };
                let before = cache.len();
                for (k, v) in lists {
                    cache.insert(k, Arc::new(v));
                }
                if cache.len() == before && !cache.contains_key(&open) {
                    return;
                }
            }
        }
    }
}

/// Everything a file goes through, one step at a time (`step` says which, for a report). Returns the properties that
/// didn't hold.
fn work(data: &[u8], step: &Mutex<String>, ticks: &AtomicU64) -> Vec<String> {
    let mut wrong = Vec::new();
    let mut spans = Vec::new();
    let at = |what: String| {
        *step.lock().unwrap() = what;
        ticks.fetch_add(1, Ordering::Relaxed);
    };
    for &lang in Lang::ALL.iter().chain([Lang::Results].iter()) {
        at(format!("lexing as {}", lang.label()));
        spans.clear();
        let _ = highlight::lex(lang, data, State::START, Some(&mut spans));
        // (in two pieces, after a line break, as coloring a long file does)
        if let Some(cut) = memchr::memchr(b'\n', data) {
            let st = highlight::lex(lang, &data[..cut + 1], State::START, None);
            let _ = highlight::lex(lang, &data[cut + 1..], st, None);
        }
        let _ = highlight::guess(lang, data, true);
    }
    // (in three pieces, each after a line break: the state at the end is the one lexing it whole ends in)
    let breaks: Vec<usize> = memchr::memchr_iter(b'\n', data).collect();
    if breaks.len() >= 2 {
        let (c1, c2) = (breaks[breaks.len() / 3] + 1, breaks[breaks.len() * 2 / 3] + 1);
        if c1 < c2 {
            for &lang in Lang::ALL.iter().chain([Lang::Results].iter()) {
                at(format!("lexing as {} in three pieces", lang.label()));
                let whole = highlight::lex(lang, data, State::START, None);
                let s = highlight::lex(lang, &data[..c1], State::START, None);
                let s = highlight::lex(lang, &data[c1..c2], s, None);
                if highlight::lex(lang, &data[c2..], s, None) != whole {
                    wrong.push(format!("lexing as {} in three pieces (cut at {c1} and {c2}) ends elsewhere", lang.label()));
                }
            }
        }
    }
    let mut doc = Document::from_text(data);
    let snap = doc.snapshot();
    let ctx = Ctx { cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)) };
    for mode in [Mode::Validate, Mode::Pretty, Mode::Minify] {
        at(format!("JSON {mode:?}"));
        let mut out = Vec::new();
        let _ = json::run(&snap, mode, b"  ", b"\n", Some(&mut out), None, &ctx);
        at(format!("XML {mode:?}"));
        let mut out = Vec::new();
        let _ = xml::run(&snap, mode, b"  ", b"\n", Some(&mut out), None, &ctx);
    }
    let len = data.len() as u64;
    for is_xml in [false, true] {
        for offset in [0, len / 3, len / 2, len.saturating_sub(1)] {
            at(format!("{} path at {offset}", if is_xml { "XML" } else { "JSON" }));
            path(&doc, offset, is_xml);
        }
    }
    for d in [Dialect { sep: b',', strict: true }, Dialect { sep: b';', strict: true }, Dialect { sep: b'\t', strict: false }] {
        at(format!("CSV Check with {:?}", d.sep as char));
        let _ = csv::check_bytes(data, d);
        at(format!("CSV Sort by column with {:?}", d.sep as char));
        for (col, desc, day_first) in [(0, false, true), (1, true, false)] {
            let _ = csv::sort_by_column(data, d, col, desc, day_first, true, &mut |_| Ok(()));
        }
    }
    let _ = csv::sniff(data);
    for op in [LineOp::SortAsc, LineOp::SortDesc, LineOp::Dedupe, LineOp::RemoveBlank, LineOp::TrimTrailing] {
        at(format!("lines {op:?}"));
        let _ = lines::apply(op, data);
    }
    // (queries from the input itself, so they match somewhere: its start, and a piece from its middle)
    let mid = data.len() / 2;
    for q in [&data[..data.len().min(1 + data.len() % 12)], &data[mid..data.len().min(mid + 1 + mid % 7)]] {
        let q = String::from_utf8_lossy(q).into_owned();
        for (regex, match_case, whole_word) in [(false, false, false), (false, true, true), (true, false, false), (true, true, true)] {
            at(format!("search {q:?}, regex {regex}, case {match_case}, word {whole_word}"));
            if let Ok(m) = Matcher::new(&Query { text: q.clone(), match_case, whole_word, regex }) {
                let _ = search::count_all(&m, &snap, &ctx);
            }
        }
    }
    at("encodings".into());
    let _ = text::detect_encoding(data, false);
    let _ = text::detect_eol(data);
    let _ = text::ansi_fits(data, false);
    for be in [false, true] {
        let decode = |bytes: &[u8]| {
            let (mut d, mut out) = (Utf16Decoder::new(be), Vec::new());
            d.push(bytes, &mut out);
            d.finish(&mut out);
            out
        };
        let utf8 = decode(data);
        let (mut e, mut back) = (Utf16Encoder::new(be), Vec::new());
        e.push(&utf8, &mut back);
        e.finish(&mut back);
        if decode(&back) != utf8 {
            wrong.push(format!("UTF-16 ({}) there and back changes the text", if be { "big-endian" } else { "little-endian" }));
        }
    }
    let (mut d, mut ansi) = (AnsiDecoder::new(), Vec::new());
    d.push(data, &mut ansi);
    d.finish(&mut ansi);
    let (mut e, mut back) = (AnsiEncoder::new(), Vec::new());
    e.push(&ansi, &mut back);
    e.finish(&mut back);
    let _ = text::ansi_round_trips(&ansi, data);
    let _ = text::utf8_to_ansi(data);
    at("display".into());
    let (mut out, mut map) = (Vec::new(), Vec::new());
    text::decode_display(data, &mut out, &mut map);
    if map.len() != out.len() + 1 {
        wrong.push("decode_display's map isn't one longer than its text".into());
    }
    let mut i = 0;
    while i < data.len() {
        i += text::cluster_len_at(&data[i..]).max(1);
    }
    let _ = text::cluster_len_before(data);
    let _ = text::backspace_len(data);
    at("Convert".into());
    let _ = crate::convert::json_unescape(data);
    let _ = crate::convert::base64_decode(data);
    let _ = crate::convert::url_decode(data);
    if crate::convert::base64_decode(&crate::convert::base64_encode(data)).ok().as_deref() != Some(data) {
        wrong.push("Base64 there and back changes the bytes".into());
    }
    if crate::convert::url_decode(crate::convert::url_encode(data).as_bytes()).ok().as_deref() != Some(data) {
        wrong.push("URL encoding there and back changes the bytes".into());
    }
    at("EditorConfig globs".into());
    let glob = String::from_utf8_lossy(&data[..data.len().min(300)]).into_owned();
    for name in ["a", "b2.txt", "src/main.rs", "x/y/z.json", ".editorconfig"] {
        let _ = crate::editorconfig::glob_match(&glob, name);
    }
    at("settings.json".into());
    let _ = crate::settings::Settings::from_json(data);
    at("shortcuts.txt".into());
    for (_, combos) in crate::shortcuts::parse(&String::from_utf8_lossy(&data[..data.len().min(4096)])).keys {
        for c in combos {
            if crate::shortcuts::Combo::parse(&c.text()) != Some(c) {
                wrong.push(format!("the key {} doesn't read back the same", c.text()));
            }
        }
    }
    at("multiple cursors".into());
    carets(data, &mut wrong);
    wrong
}

/// Cursors put where the input's bytes say, then typing, deleting, Enter and moving at all of them.
fn carets(data: &[u8], wrong: &mut Vec<String>) {
    use crate::core::document::{EditKind, Sel};
    use crate::edit::{CaretMove, Carets, MultiEdit};
    let text = &data[..data.len().min(64 << 10)];
    let mut doc = Document::from_text(text);
    let len = doc.len();
    let at = |k: usize| crate::edit::char_start(&doc, (data.get(k).copied().unwrap_or(0) as u64 * 7919 + k as u64 * 31) % (len + 1));
    let mut main = Sel::at(at(0));
    let mut c = Carets { more: (1..9).map(|k| Sel::at(at(k))).collect(), version: doc.version, col: None };
    c.tidy(&mut main);
    let steps: [(&str, MultiEdit, EditKind); 4] = [
        ("type", MultiEdit::Put(vec![b"x\xC3\xA9"]), EditKind::Typing),
        ("Backspace", MultiEdit::Delete { back: true, word: false }, EditKind::Backspace),
        ("Delete a word", MultiEdit::Delete { back: false, word: true }, EditKind::Other),
        ("Enter", MultiEdit::Newline, EditKind::Other),
    ];
    for (name, what, kind) in steps {
        crate::edit::multi_edit(&mut doc, &mut main, &mut c, what, kind);
        crate::edit::move_carets(&doc, &mut main, &mut c, if name == "Enter" { CaretMove::Up } else { CaretMove::Right }, name == "type", 4);
        c.tidy(&mut main);
        let all = c.ordered(main);
        if all.iter().any(|(s, _)| s.anchor > doc.len() || s.caret > doc.len()) {
            wrong.push(format!("a cursor past the end after {name}"));
        }
        if all.windows(2).any(|w| w[1].0.start() < w[0].0.end() || w[0].0 == w[1].0) {
            wrong.push(format!("cursors overlap after {name}"));
        }
        if c.version != doc.version {
            wrong.push(format!("the cursors' version isn't the text's after {name}"));
        }
    }
}

#[test]
#[ignore]
fn fuzz_hostile_files() {
    let rounds: u64 = std::env::var("SLATE_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    let seed: u64 = std::env::var("SLATE_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5EED_51A7E);
    let step = Arc::new(Mutex::new(String::new()));
    let ticks = Arc::new(AtomicU64::new(0));
    let current = Arc::new(Mutex::new(Vec::<u8>::new()));
    // The watchdog: a step that doesn't move on for 10 s is a hang.
    {
        let (step, ticks, current) = (step.clone(), ticks.clone(), current.clone());
        std::thread::spawn(move || {
            let mut last = u64::MAX;
            loop {
                std::thread::sleep(Duration::from_secs(10));
                let now = ticks.load(Ordering::Relaxed);
                if now == last {
                    let file = std::env::temp_dir().join("slate-fuzz-hang.bin");
                    let _ = std::fs::write(&file, &*current.lock().unwrap());
                    eprintln!("HANG while {} (input: {})", step.lock().unwrap(), file.display());
                    std::process::exit(2);
                }
                last = now;
            }
        });
    }
    let mut r = Rng(seed | 1);
    let mut panics = Vec::new();
    let mut wrong = Vec::new();
    let mut bytes = 0usize;
    for i in 0..rounds {
        let data = input(&mut r);
        bytes += data.len();
        *current.lock().unwrap() = data.clone();
        match catch_unwind(AssertUnwindSafe(|| work(&data, &step, &ticks))) {
            Ok(w) if w.is_empty() => {}
            r => {
                let file = std::env::temp_dir().join(format!("slate-fuzz-{i}.bin"));
                let _ = std::fs::write(&file, &data);
                match r {
                    Ok(w) => wrong.extend(w.into_iter().map(|w| format!("{w} ({})", file.display()))),
                    Err(_) => panics.push(format!("{} ({})", step.lock().unwrap(), file.display())),
                }
            }
        }
    }
    eprintln!("{rounds} inputs, {} MB, {} panics, {} properties that didn't hold", bytes >> 20, panics.len(), wrong.len());
    assert!(panics.is_empty() && wrong.is_empty(), "{panics:#?}\n{wrong:#?}");
}
