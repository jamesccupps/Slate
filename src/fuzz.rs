//! Hostile files (`cargo test --release --lib fuzz -- --ignored --nocapture`): random and mutated text through every
//! language's lexer (whole, and in two pieces as coloring does), JSON and XML Check and Format, and the structure
//! panel's scans and paths. A panic is reported with its input. An input that keeps one step busy for 10 s (a loop
//! that doesn't end: a file that would freeze the window) stops the run, its bytes written to `slate-fuzz-hang.bin`
//! in the temp folder. `SLATE_FUZZ_ROUNDS` sets how many inputs (default 2000), `SLATE_FUZZ_SEED` where they start.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::document::Document;
use crate::core::job::Ctx;
use crate::core::json::{self, Mode};
use crate::core::jsonnav::{self, Children};
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

/// Everything a file goes through, one step at a time (`step` says which, for a report).
fn work(data: &[u8], step: &Mutex<String>, ticks: &AtomicU64) {
    let mut spans = Vec::new();
    let at = |what: String| {
        *step.lock().unwrap() = what;
        ticks.fetch_add(1, Ordering::Relaxed);
    };
    for &lang in Lang::ALL.iter() {
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
    let mut bytes = 0usize;
    for i in 0..rounds {
        let data = input(&mut r);
        bytes += data.len();
        *current.lock().unwrap() = data.clone();
        if catch_unwind(AssertUnwindSafe(|| work(&data, &step, &ticks))).is_err() {
            let file = std::env::temp_dir().join(format!("slate-fuzz-panic-{i}.bin"));
            let _ = std::fs::write(&file, &data);
            panics.push(format!("{} ({})", step.lock().unwrap(), file.display()));
        }
    }
    eprintln!("{rounds} inputs, {} MB, {} panics", bytes >> 20, panics.len());
    assert!(panics.is_empty(), "{panics:#?}");
}
