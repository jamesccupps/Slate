//! Streaming JSON tools: pretty-print, minify and validate documents of any size in one pass (output goes to a
//! writer, e.g. a temp file). Several top-level values in a row (JSON Lines / NDJSON) are accepted; each one
//! goes on its own line. Comments (JSONC, like VS Code's settings) are refused with a message saying so, as
//! formatting would have to drop or move them.

use std::io::{self, Write};

use super::buffer::Snapshot;
use super::job::Ctx;
use super::source::IndexBuilder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Pretty,
    Minify,
    Validate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    pub offset: u64,
    pub msg: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Number of top-level values (more than one means JSON Lines).
    pub values: u64,
    pub max_depth: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Num {
    Minus,
    Zero,
    Int,
    Dot,
    Frac,
    Exp,
    ExpSign,
    ExpInt,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Value,
    ValueOrClose,
    KeyOrClose,
    Key,
    Colon,
    After,
    Str { key: bool },
    Esc { key: bool },
    Hex { key: bool, n: u8 },
    Num(Num),
    Lit { word: &'static [u8], i: u8 },
    /// After a `/` outside a string at the end of a chunk: a comment if `/` or `*` follows.
    Slash,
}

/// The message when the text has `//` or `/* */` comments, which plain JSON doesn't allow.
pub const COMMENTS: &str = "It has comments (JSONC), which plain JSON doesn't allow";

/// Deeper levels aren't indented any further, so absurdly deep input can't turn into gigabytes of spaces.
const MAX_INDENT: usize = 100;

pub struct Formatter<'w> {
    mode: Mode,
    st: St,
    /// true = object, false = array
    stack: Vec<bool>,
    /// An opening bracket not written yet (so empty containers come out as `{}` / `[]`).
    pending_open: Option<u8>,
    indent: Vec<u8>,
    eol: Vec<u8>,
    out: Vec<u8>,
    w: Option<&'w mut dyn Write>,
    idx: Option<&'w mut IndexBuilder>,
    stats: Stats,
    pos: u64,
    io_err: Option<io::Error>,
    /// Whitespace seen since the last top-level value ended.
    gap: bool,
    /// The last top-level value was a number, true, false or null (which need a space before the next value).
    bare: bool,
    /// Where the `/` of `St::Slash` is, and the state before it.
    slash_at: u64,
    before_slash: St,
}

fn err(offset: u64, msg: impl Into<String>) -> JsonError {
    JsonError { offset, msg: msg.into() }
}

fn describe(b: u8) -> String {
    if b.is_ascii_graphic() { format!("'{}'", b as char) } else { format!("byte 0x{b:02X}") }
}

impl<'w> Formatter<'w> {
    /// `indent` is the indentation unit (e.g. two spaces or a tab), `eol` the line ending to use.
    pub fn new(
        mode: Mode,
        indent: &[u8],
        eol: &[u8],
        w: Option<&'w mut dyn Write>,
        idx: Option<&'w mut IndexBuilder>,
    ) -> Self {
        Formatter {
            mode,
            st: St::Value,
            stack: Vec::new(),
            pending_open: None,
            indent: indent.to_vec(),
            eol: eol.to_vec(),
            out: Vec::with_capacity(1 << 20),
            w,
            idx,
            stats: Stats::default(),
            pos: 0,
            io_err: None,
            gap: false,
            bare: false,
            slash_at: 0,
            before_slash: St::Value,
        }
    }

    fn emit(&mut self, b: &[u8]) {
        if self.mode != Mode::Validate {
            self.out.extend_from_slice(b);
            if self.out.len() >= 1 << 20 {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        if self.out.is_empty() {
            return;
        }
        if let Some(i) = self.idx.as_mut() {
            i.push(&self.out);
        }
        if let Some(w) = self.w.as_mut() {
            if let Err(e) = w.write_all(&self.out) {
                self.io_err.get_or_insert(e);
            }
        }
        self.out.clear();
    }

    fn newline(&mut self, depth: usize) {
        if self.mode == Mode::Pretty {
            let eol = std::mem::take(&mut self.eol);
            self.emit(&eol);
            self.eol = eol;
            let ind = std::mem::take(&mut self.indent);
            for _ in 0..depth.min(MAX_INDENT) {
                self.emit(&ind);
            }
            self.indent = ind;
        }
    }

    /// The error for an unexpected byte `b` at `at` in state `st` (between tokens).
    fn bad(&self, st: St, b: u8, at: u64) -> JsonError {
        match st {
            St::Colon => err(at, "Expected ':' after the key"),
            St::Key | St::KeyOrClose => err(at, format!("Expected a key in quotes, found {}", describe(b))),
            // `01` or `truefalse` is a mistake, not two values
            St::After if self.stack.is_empty() && self.bare && !self.gap => {
                err(at, format!("Unexpected {} after the value", describe(b)))
            }
            St::After if !self.stack.is_empty() => {
                let close = if *self.stack.last().unwrap() { '}' } else { ']' };
                err(at, format!("Expected ',' or '{close}', found {}", describe(b)))
            }
            _ => err(at, format!("Expected a value, found {}", describe(b))),
        }
    }

    /// Before a value or key: writes a pending `{`/`[` and the line break after it.
    fn value_start(&mut self) {
        if self.stack.is_empty() && self.pending_open.is_none() {
            self.gap = false;
        }
        if let Some(c) = self.pending_open.take() {
            self.emit(&[c]);
            self.newline(self.stack.len());
        } else if self.stack.is_empty() {
            if self.stats.values > 0 && self.mode != Mode::Validate {
                let eol = std::mem::take(&mut self.eol);
                self.emit(&eol);
                self.eol = eol;
            }
            self.stats.values += 1;
        }
    }

    fn open(&mut self, c: u8, obj: bool) {
        self.value_start();
        self.pending_open = Some(c);
        self.stack.push(obj);
        self.stats.max_depth = self.stats.max_depth.max(self.stack.len());
    }

    fn close(&mut self, c: u8, obj: bool, at: u64) -> Result<(), JsonError> {
        match self.stack.pop() {
            Some(o) if o == obj => {}
            Some(true) => return Err(err(at, "Expected '}' to close the object")),
            Some(false) => return Err(err(at, "Expected ']' to close the array")),
            None => return Err(err(at, format!("Unexpected {}", describe(c)))),
        }
        if let Some(open) = self.pending_open.take() {
            self.emit(&[open, c]);
        } else {
            self.newline(self.stack.len());
            self.emit(&[c]);
        }
        self.st = St::After;
        Ok(())
    }

    fn end_number(&mut self, n: Num, at: u64) -> Result<(), JsonError> {
        match n {
            Num::Zero | Num::Int | Num::Frac | Num::ExpInt => {
                self.st = St::After;
                Ok(())
            }
            _ => Err(err(at, "Invalid number")),
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<(), JsonError> {
        let base = self.pos;
        let mut i = 0usize;
        while i < data.len() {
            let at = base + i as u64;
            let b = data[i];
            match self.st {
                St::Str { key } => {
                    let rest = &data[i..];
                    let j = memchr::memchr2(b'"', b'\\', rest).unwrap_or(rest.len());
                    if let Some(k) = rest[..j].iter().position(|&c| c < 0x20) {
                        return Err(err(at + k as u64, "Line break or control character inside a string"));
                    }
                    self.emit(&rest[..j]);
                    i += j;
                    if i < data.len() {
                        if data[i] == b'"' {
                            self.emit(b"\"");
                            self.st = if key { St::Colon } else { St::After };
                        } else {
                            self.emit(b"\\");
                            self.st = St::Esc { key };
                        }
                        i += 1;
                    }
                    continue;
                }
                St::Esc { key } => {
                    match b {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.st = St::Str { key },
                        b'u' => self.st = St::Hex { key, n: 0 },
                        _ => return Err(err(at, "Invalid escape in a string")),
                    }
                    self.emit(&[b]);
                }
                St::Hex { key, n } => {
                    if !b.is_ascii_hexdigit() {
                        return Err(err(at, "Invalid \\u escape in a string"));
                    }
                    self.emit(&[b]);
                    self.st = if n == 3 { St::Str { key } } else { St::Hex { key, n: n + 1 } };
                }
                St::Num(n) => {
                    let next = match (n, b) {
                        (Num::Minus, b'0') => Some(Num::Zero),
                        (Num::Minus, b'1'..=b'9') => Some(Num::Int),
                        (Num::Int, b'0'..=b'9') => Some(Num::Int),
                        (Num::Zero | Num::Int, b'.') => Some(Num::Dot),
                        (Num::Dot | Num::Frac, b'0'..=b'9') => Some(Num::Frac),
                        (Num::Zero | Num::Int | Num::Frac, b'e' | b'E') => Some(Num::Exp),
                        (Num::Exp, b'+' | b'-') => Some(Num::ExpSign),
                        (Num::Exp | Num::ExpSign | Num::ExpInt, b'0'..=b'9') => Some(Num::ExpInt),
                        _ => None,
                    };
                    match next {
                        Some(s) => {
                            self.st = St::Num(s);
                            self.emit(&[b]);
                        }
                        None => {
                            self.end_number(n, at)?;
                            continue; // look at this byte again
                        }
                    }
                }
                St::Lit { word, i: k } => {
                    if b != word[k as usize] {
                        return Err(err(at, "Unknown word (expected true, false or null)"));
                    }
                    self.emit(&[b]);
                    self.st = if k as usize + 1 == word.len() { St::After } else { St::Lit { word, i: k + 1 } };
                }
                St::Slash => {
                    return Err(if matches!(b, b'/' | b'*') {
                        err(self.slash_at, COMMENTS)
                    } else {
                        self.bad(self.before_slash, b'/', self.slash_at)
                    });
                }
                st => {
                    if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                        if self.stack.is_empty() {
                            self.gap = true;
                        }
                        i += 1;
                        continue;
                    }
                    if b == b'/' {
                        // `//` or `/*` starts a comment (JSONC); any other `/` is a mistake
                        match data.get(i + 1) {
                            Some(b'/' | b'*') => return Err(err(at, COMMENTS)),
                            Some(_) => return Err(self.bad(st, b, at)),
                            None => {
                                self.before_slash = st;
                                self.slash_at = at;
                                self.st = St::Slash;
                                i += 1;
                                continue;
                            }
                        }
                    }
                    match (st, b) {
                        (St::Value | St::ValueOrClose, b'{') => {
                            if self.stack.is_empty() {
                                self.bare = false;
                            }
                            self.open(b'{', true);
                            self.st = St::KeyOrClose;
                        }
                        (St::Value | St::ValueOrClose, b'[') => {
                            if self.stack.is_empty() {
                                self.bare = false;
                            }
                            self.open(b'[', false);
                            self.st = St::ValueOrClose;
                        }
                        (St::ValueOrClose, b']') | (St::After, b']') => self.close(b']', false, at)?,
                        (St::KeyOrClose, b'}') | (St::After, b'}') => self.close(b'}', true, at)?,
                        (St::Value | St::ValueOrClose, b'"') => {
                            if self.stack.is_empty() {
                                self.bare = false;
                            }
                            self.value_start();
                            self.emit(b"\"");
                            self.st = St::Str { key: false };
                        }
                        (St::KeyOrClose | St::Key, b'"') => {
                            self.value_start();
                            self.emit(b"\"");
                            self.st = St::Str { key: true };
                        }
                        (St::Value | St::ValueOrClose, b'-' | b'0'..=b'9') => {
                            self.bare = self.stack.is_empty();
                            self.value_start();
                            self.emit(&[b]);
                            self.st = St::Num(match b {
                                b'-' => Num::Minus,
                                b'0' => Num::Zero,
                                _ => Num::Int,
                            });
                        }
                        (St::Value | St::ValueOrClose, b't' | b'f' | b'n') => {
                            self.bare = self.stack.is_empty();
                            self.value_start();
                            self.emit(&[b]);
                            let word: &'static [u8] = match b {
                                b't' => b"true",
                                b'f' => b"false",
                                _ => b"null",
                            };
                            self.st = St::Lit { word, i: 1 };
                        }
                        (St::Colon, b':') => {
                            self.emit(if self.mode == Mode::Pretty { b": " } else { b":" });
                            self.st = St::Value;
                        }
                        (St::After, b',') if !self.stack.is_empty() => {
                            self.emit(b",");
                            self.newline(self.stack.len());
                            self.st = if *self.stack.last().unwrap() { St::Key } else { St::Value };
                        }
                        (St::After, _) if self.stack.is_empty() && !(self.bare && !self.gap) => {
                            // Another top-level value (JSON Lines).
                            self.st = St::Value;
                            continue;
                        }
                        _ => return Err(self.bad(st, b, at)),
                    }
                }
            }
            i += 1;
        }
        self.pos += data.len() as u64;
        if let Some(e) = self.io_err.take() {
            return Err(err(self.pos, format!("Couldn't write the result: {e}")));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Stats, JsonError> {
        let at = self.pos;
        if let St::Num(n) = self.st {
            self.end_number(n, at)?;
        }
        match self.st {
            St::After if self.stack.is_empty() => {}
            St::Value if self.stack.is_empty() && self.stats.values > 0 => {}
            St::Value if self.stack.is_empty() => return Err(err(at, "There is no JSON here")),
            St::Str { .. } | St::Esc { .. } | St::Hex { .. } => {
                return Err(err(at, "The file ends inside a string"));
            }
            St::Slash => return Err(self.bad(self.before_slash, b'/', self.slash_at)),
            _ => return Err(err(at, "The file ends before the JSON is complete")),
        }
        if self.mode == Mode::Pretty && self.stats.values > 0 {
            let eol = self.eol.clone();
            self.emit(&eol);
        }
        self.flush();
        if let Some(e) = self.io_err.take() {
            return Err(err(at, format!("Couldn't write the result: {e}")));
        }
        Ok(self.stats)
    }
}

/// Runs `mode` over the whole snapshot.
pub fn run<'w>(
    snap: &Snapshot,
    mode: Mode,
    indent: &[u8],
    eol: &[u8],
    w: Option<&'w mut dyn Write>,
    idx: Option<&'w mut IndexBuilder>,
    ctx: &Ctx,
) -> Result<Stats, JsonError> {
    let mut f = Formatter::new(mode, indent, eol, w, idx);
    let mut pos = 0u64;
    let mut res = Ok(());
    while pos < snap.len() && res.is_ok() {
        if ctx.cancelled() {
            return Err(err(pos, "Cancelled"));
        }
        let end = (pos + (8 << 20)).min(snap.len());
        snap.chunks(pos, end, &mut |c| {
            res = f.feed(c);
            res.is_ok()
        });
        pos = end;
        ctx.set(pos);
    }
    res?;
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(mode: Mode, input: &str) -> Result<String, JsonError> {
        let mut out = Vec::new();
        {
            let mut f = Formatter::new(mode, b"  ", b"\n", Some(&mut out), None);
            // feed byte by byte to exercise chunk boundaries
            for b in input.as_bytes() {
                f.feed(std::slice::from_ref(b))?;
            }
            f.finish()?;
        }
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn pretty_and_minify() {
        let src = concat!(r#"{"a":1,"b":[true,false,null,-1.5e+3,"x\"y"#, "\\u00e9", r#""],"c":{},"d":[],"e":[{}]}"#);
        let pretty = fmt(Mode::Pretty, src).unwrap();
        assert_eq!(
            pretty,
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    false,\n    null,\n    -1.5e+3,\n    \"x\\\"y\\u00e9\"\n  ],\n  \"c\": {},\n  \"d\": [],\n  \"e\": [\n    {}\n  ]\n}\n"
        );
        assert_eq!(fmt(Mode::Minify, &pretty).unwrap(), src);
        // the pretty output parses as the same value
        let a: serde_json::Value = serde_json::from_str(src).unwrap();
        let b: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn json_lines() {
        let out = fmt(Mode::Minify, "{\"a\": 1}\n{\"a\": 2}\n\n[3]\n").unwrap();
        assert_eq!(out, "{\"a\":1}\n{\"a\":2}\n[3]");
        let mut f = Formatter::new(Mode::Validate, b"", b"\n", None, None);
        f.feed(b"1 2 \"three\"").unwrap();
        assert_eq!(f.finish().unwrap().values, 3);
        // values glued together are mistakes, except after a string or a closing bracket
        for bad in ["01", "-01", "1-2", "truefalse", "0.5e3-1", "null1"] {
            assert!(fmt(Mode::Validate, bad).is_err(), "{bad}");
        }
        for ok in ["{}{}", "[1][2]", "\"a\"\"b\"", "{\"a\":1}\n2"] {
            assert!(fmt(Mode::Validate, ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = fmt(Mode::Validate, "{\"a\": 1 \"b\": 2}").unwrap_err();
        assert_eq!(e.offset, 8);
        assert!(e.msg.contains("Expected ','"), "{}", e.msg);
        assert_eq!(fmt(Mode::Validate, "[1, 2").unwrap_err().msg, "The file ends before the JSON is complete");
        assert!(fmt(Mode::Validate, "[01]").is_err());
        assert!(fmt(Mode::Validate, "[1.]").is_err());
        assert!(fmt(Mode::Validate, "[tru]").is_err());
        assert!(fmt(Mode::Validate, "{\"a\" 1}").unwrap_err().msg.contains("':'"));
        assert!(fmt(Mode::Validate, "\"a\nb\"").is_err());
        assert!(fmt(Mode::Validate, "[1}").is_err());
        assert!(fmt(Mode::Validate, "").is_err());
        assert!(fmt(Mode::Validate, "{\"a\":1,}").is_err());
        assert_eq!(fmt(Mode::Validate, "  [1, {\"x\": [2]}]  ").unwrap(), "");
    }

    #[test]
    fn numbers_at_end_of_input() {
        assert_eq!(fmt(Mode::Minify, "12").unwrap(), "12");
        assert_eq!(fmt(Mode::Minify, "[0, -0.5, 1e9]").unwrap(), "[0,-0.5,1e9]");
        assert!(fmt(Mode::Minify, "-").is_err());
    }

    #[test]
    fn comments_are_refused_clearly() {
        // (fed byte by byte, so the `/` always ends a chunk)
        for (src, at) in [("{\n  // size\n  \"a\": 1\n}", 4), ("/* c */ {}", 0), ("[1, /* x */ 2]", 4), ("{\"a\": 1} // end", 9)] {
            let e = fmt(Mode::Validate, src).unwrap_err();
            assert_eq!((e.offset, e.msg.as_str()), (at, COMMENTS), "{src}");
            let mut f = Formatter::new(Mode::Pretty, b"  ", b"\n", None, None);
            assert_eq!(f.feed(src.as_bytes()).unwrap_err().msg, COMMENTS, "{src} in one piece");
        }
        // any other `/` is just a mistake
        assert_eq!(fmt(Mode::Validate, "[1/2]").unwrap_err().msg, "Expected ',' or ']', found '/'");
        assert_eq!(fmt(Mode::Validate, "{\"a\":1}/").unwrap_err(), err(7, "Expected a value, found '/'"));
        assert_eq!(fmt(Mode::Validate, "{/}").unwrap_err().msg, "Expected a key in quotes, found '/'");
        assert_eq!(fmt(Mode::Validate, "1/").unwrap_err().msg, "Unexpected '/' after the value");
        // a slash inside a string is fine
        assert_eq!(fmt(Mode::Minify, "{\"url\": \"http://x/*y*/\"}").unwrap(), "{\"url\":\"http://x/*y*/\"}");
    }

    #[test]
    fn deep_nesting_stops_indenting() {
        let s = format!("{}{}", "[".repeat(150), "]".repeat(150));
        let p = fmt(Mode::Pretty, &s).unwrap();
        let widest = p.lines().map(|l| l.len() - l.trim_start().len()).max().unwrap();
        assert_eq!(widest, MAX_INDENT * 2);
        assert_eq!(fmt(Mode::Minify, &p).unwrap(), s);
    }
}
