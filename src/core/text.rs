//! Text helpers: encoding and line-ending detection, conversions, decoding bytes for display, and character
//! classes for word movement. Documents are stored as UTF-8 internally; files in other encodings are converted
//! when opened and converted back when saved.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
#[cfg(windows)]
use windows::Win32::Globalization::{
    CPINFO, GetACP, GetCPInfo, IsDBCSLeadByteEx, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
    WC_NO_BEST_FIT_CHARS, WideCharToMultiByte,
};
#[cfg(windows)]
use windows::core::PCSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// The system's legacy code page ("ANSI", usually Windows-1252).
    Ansi,
}

impl Encoding {
    pub fn label(self) -> String {
        match self {
            Encoding::Utf8 => "UTF-8".into(),
            Encoding::Utf8Bom => "UTF-8 with BOM".into(),
            Encoding::Utf16Le => "UTF-16 LE".into(),
            Encoding::Utf16Be => "UTF-16 BE".into(),
            Encoding::Ansi => format!("ANSI ({})", ansi_name()),
        }
    }
    pub fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8Bom => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            _ => &[],
        }
    }
    /// Whether the file bytes are the document bytes (no conversion on open or save).
    pub fn is_native(self) -> bool {
        matches!(self, Encoding::Utf8 | Encoding::Utf8Bom)
    }
}

/// The code page "ANSI" stands for: the system's, except with the "Use Unicode UTF-8 for worldwide language
/// support" option (code page 65001), where files that aren't UTF-8 are taken as Windows-1252, the code page most
/// of them were written in (and one in which every byte converts back to itself). Elsewhere (Linux), where text is
/// UTF-8, files that aren't are taken as Windows-1252 too.
pub fn ansi_codepage() -> u32 {
    #[cfg(windows)]
    return effective_codepage(unsafe { GetACP() });
    #[cfg(not(windows))]
    return 1252;
}

#[cfg(windows)]
fn effective_codepage(acp: u32) -> u32 {
    if acp == 65001 { 1252 } else { acp }
}

fn ansi_name() -> String {
    match ansi_codepage() {
        1252 => "Windows-1252".into(),
        1250 => "Windows-1250".into(),
        1251 => "Windows-1251".into(),
        932 => "Shift-JIS".into(),
        936 => "GBK".into(),
        949 => "EUC-KR".into(),
        950 => "Big5".into(),
        cp => format!("code page {cp}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Eol {
    Crlf,
    Lf,
}

impl Eol {
    /// The system's: what new documents, and text without any line break, get.
    pub const NATIVE: Eol = if cfg!(windows) { Eol::Crlf } else { Eol::Lf };

    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Eol::Crlf => b"\r\n",
            Eol::Lf => b"\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Eol::Crlf => "Windows (CRLF)",
            Eol::Lf => "Unix (LF)",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Eol::Crlf => "CRLF",
            Eol::Lf => "LF",
        }
    }
}

/// Guesses the encoding from the start of a file. Returns the encoding and the length of its byte order mark.
pub fn detect_encoding(sample: &[u8], truncated: bool) -> (Encoding, usize) {
    if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (Encoding::Utf8Bom, 3);
    }
    if sample.starts_with(&[0xFF, 0xFE]) {
        return (Encoding::Utf16Le, 2);
    }
    if sample.starts_with(&[0xFE, 0xFF]) {
        return (Encoding::Utf16Be, 2);
    }
    // UTF-16 without a BOM: mostly-ASCII text has a zero in every other byte (and reads as UTF-16, but for a few
    // halves of characters without their other half: a damaged file is still text, see `Utf16Decoder::bad`).
    let n = sample.len().min(8192) & !1;
    if n >= 16 {
        let (mut even, mut odd) = (0usize, 0usize);
        for i in (0..n).step_by(2) {
            even += (sample[i] == 0) as usize;
            odd += (sample[i + 1] == 0) as usize;
        }
        let half = n / 2;
        if odd * 10 > half * 4 && even * 20 < half && utf16_mostly(sample, n, false) {
            return (Encoding::Utf16Le, 0);
        }
        if even * 10 > half * 4 && odd * 20 < half && utf16_mostly(sample, n, true) {
            return (Encoding::Utf16Be, 0);
        }
    }
    match std::str::from_utf8(sample) {
        Ok(_) => (Encoding::Utf8, 0),
        // A sequence cut off by the end of the sample is fine.
        Err(e) if e.error_len().is_none() && truncated => (Encoding::Utf8, 0),
        // Mostly UTF-8 with a few bad bytes (a damaged file, a line pasted in from elsewhere): as UTF-8 every byte
        // stays as it is (the bad ones show as symbols), where reading it all as ANSI would garble each character.
        Err(_) if mostly_utf8(sample, truncated) => (Encoding::Utf8, 0),
        Err(_) => (Encoding::Ansi, 0),
    }
}

/// Whether the first `n` (even) bytes of `sample` read as UTF-16 but for a few unpaired surrogates (at most one
/// unit in 32; a pair cut off at `n` may go on after it). Only these bytes count, so a file's first 8 KiB decide
/// for opening it and for `io::looks_binary` alike.
fn utf16_mostly(sample: &[u8], n: usize, be: bool) -> bool {
    let (mut bad, mut high) = (0, false);
    for p in sample[..n].chunks_exact(2) {
        let u = if be { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) };
        let low = (0xDC00..0xE000).contains(&u);
        bad += (high != low) as usize;
        high = (0xD800..0xDC00).contains(&u);
    }
    bad * 32 <= n / 2
}

/// Whether `bytes` has some multi-byte UTF-8 characters, and at least as many of them as invalid sequences.
fn mostly_utf8(bytes: &[u8], truncated: bool) -> bool {
    let (mut good, mut bad) = (0usize, 0usize);
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                good += s.bytes().filter(|&b| b >= 0xC0).count();
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                good += rest[..valid].iter().filter(|&&b| b >= 0xC0).count();
                match e.error_len() {
                    Some(n) => {
                        bad += 1;
                        rest = &rest[valid + n..];
                    }
                    None => {
                        bad += !truncated as usize;
                        break;
                    }
                }
            }
        }
    }
    good > 0 && good >= bad
}

/// Whether `text` (decoded from `bytes` in the ANSI code page) converts back to exactly `bytes`. Only then can a
/// file be read as ANSI without saving it changing bytes nobody edited: in Windows-1252 every byte does, but in
/// the double-byte code pages (Japanese, Chinese, Korean) not every byte sequence is text.
pub fn ansi_round_trips(text: &[u8], bytes: &[u8]) -> bool {
    round_trips_cp(text, bytes, ansi_codepage())
}

fn round_trips_cp(text: &[u8], bytes: &[u8], cp: u32) -> bool {
    if let Some(t) = single_byte(cp) {
        // (one byte, one character: each byte must come back as itself)
        return bytes.iter().all(|&b| t.same[b as usize]);
    }
    let mut enc = AnsiEncoder::with_codepage(cp);
    let mut back = Vec::new();
    let mut at = 0;
    for chunk in text.chunks(1 << 20) {
        back.clear();
        enc.push(chunk, &mut back);
        if bytes.get(at..at + back.len()) != Some(&back[..]) {
            return false;
        }
        at += back.len();
    }
    back.clear();
    enc.finish(&mut back);
    !enc.lossy && bytes.get(at..) == Some(&back[..])
}

/// Whether the start of a file (`truncated`: more follows) reads as ANSI and converts back to the same bytes.
pub fn ansi_fits(sample: &[u8], truncated: bool) -> bool {
    fits_cp(sample, truncated, ansi_codepage())
}

fn fits_cp(sample: &[u8], truncated: bool, cp: u32) -> bool {
    let mut dec = AnsiDecoder::with_codepage(cp);
    let mut text = Vec::with_capacity(sample.len() + sample.len() / 2);
    dec.push(sample, &mut text);
    // (a double-byte character cut off by the end of the sample waits in the decoder: leave it out)
    let held = if truncated {
        dec.pending.take().is_some() as usize
    } else {
        dec.finish(&mut text);
        0
    };
    round_trips_cp(&text, &sample[..sample.len() - held], cp)
}

/// While a file is converted from the ANSI code page (in chunks): checks that the text converts back to exactly
/// the file's bytes.
pub struct AnsiCheck {
    enc: AnsiEncoder,
    /// File bytes that converted-back text hasn't matched yet.
    ahead: Vec<u8>,
    ok: bool,
}

impl Default for AnsiCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiCheck {
    pub fn new() -> Self {
        AnsiCheck { enc: AnsiEncoder::new(), ahead: Vec::new(), ok: true }
    }

    /// `input`: bytes given to the decoder; `text`: what it made of them.
    pub fn push(&mut self, input: &[u8], text: &[u8]) {
        if !self.ok {
            return;
        }
        if let Some(t) = &self.enc.sbcs {
            // (one byte, one character: each byte must come back as itself)
            self.ok = input.iter().all(|&b| t.same[b as usize]);
            return;
        }
        self.ahead.extend_from_slice(input);
        let mut back = Vec::with_capacity(text.len());
        self.enc.push(text, &mut back);
        self.eat(&back);
    }

    /// Whether everything so far converted back exactly.
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// After the decoder's last output (`text`): whether everything converted back exactly.
    pub fn finish(mut self, text: &[u8]) -> bool {
        if self.enc.sbcs.is_some() {
            return self.ok;
        }
        if self.ok {
            let mut back = Vec::new();
            self.enc.push(text, &mut back);
            self.enc.finish(&mut back);
            self.eat(&back);
        }
        self.ok && self.ahead.is_empty() && !self.enc.lossy
    }

    fn eat(&mut self, back: &[u8]) {
        if self.ahead.starts_with(back) {
            self.ahead.drain(..back.len());
        } else {
            self.ok = false;
        }
    }
}

/// The line ending most used in `sample`; the system's if there are none (CRLF on Windows, LF on Linux).
pub fn detect_eol(sample: &[u8]) -> Eol {
    let lf = bytecount::count(sample, b'\n');
    if lf == 0 {
        return Eol::NATIVE;
    }
    let crlf = memchr::memmem::find_iter(sample, b"\r\n").count();
    if crlf * 2 >= lf { Eol::Crlf } else { Eol::Lf }
}

/// Converts UTF-16 bytes to UTF-8, streaming: `carry` holds an odd byte or a lone high surrogate between calls.
/// Unpaired surrogates (and an odd byte at the end) become U+FFFD; `bad` counts them, as saving can't give them back.
pub struct Utf16Decoder {
    big_endian: bool,
    odd: Option<u8>,
    high: Option<u16>,
    pub bad: u64,
}

impl Utf16Decoder {
    pub fn new(big_endian: bool) -> Self {
        Utf16Decoder { big_endian, odd: None, high: None, bad: 0 }
    }

    pub fn push(&mut self, mut input: &[u8], out: &mut Vec<u8>) {
        out.reserve(input.len() / 2 * 3 + 4);
        if let Some(b) = self.odd.take() {
            if input.is_empty() {
                self.odd = Some(b);
                return;
            }
            self.emit(self.unit(b, input[0]), out);
            input = &input[1..];
        }
        let pairs = input.chunks_exact(2);
        if let [b] = pairs.remainder() {
            self.odd = Some(*b);
        }
        for p in pairs {
            let u = self.unit(p[0], p[1]);
            // (most text is ASCII)
            if u < 0x80 && self.high.is_none() {
                out.push(u as u8);
            } else {
                self.emit(u, out);
            }
        }
    }

    fn unit(&self, a: u8, b: u8) -> u16 {
        if self.big_endian { u16::from_be_bytes([a, b]) } else { u16::from_le_bytes([a, b]) }
    }

    fn emit(&mut self, u: u16, out: &mut Vec<u8>) {
        let mut buf = [0u8; 4];
        if let Some(h) = self.high.take() {
            if (0xDC00..0xE000).contains(&u) {
                let c = 0x10000 + (((h as u32) - 0xD800) << 10) + ((u as u32) - 0xDC00);
                let c = char::from_u32(c).unwrap_or('\u{FFFD}');
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                return;
            }
            self.bad += 1;
            out.extend_from_slice("\u{FFFD}".as_bytes());
        }
        if (0xD800..0xDC00).contains(&u) {
            self.high = Some(u);
        } else if (0xDC00..0xE000).contains(&u) {
            self.bad += 1;
            out.extend_from_slice("\u{FFFD}".as_bytes());
        } else {
            let c = char::from_u32(u as u32).unwrap_or('\u{FFFD}');
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.high.take().is_some() || self.odd.take().is_some() {
            self.bad += 1;
            out.extend_from_slice("\u{FFFD}".as_bytes());
        }
    }
}

/// Converts UTF-8 to UTF-16 bytes, streaming (an incomplete sequence at the end of a chunk waits for the next).
pub struct Utf16Encoder {
    big_endian: bool,
    pending: Vec<u8>,
}

impl Utf16Encoder {
    pub fn new(big_endian: bool) -> Self {
        Utf16Encoder { big_endian, pending: Vec::new() }
    }

    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let owned;
        let data: &[u8] = if self.pending.is_empty() {
            input
        } else {
            self.pending.extend_from_slice(input);
            owned = std::mem::take(&mut self.pending);
            &owned
        };
        let mut i = 0;
        while i < data.len() {
            match std::str::from_utf8(&data[i..]) {
                Ok(s) => {
                    self.put(s, out);
                    return;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    self.put(unsafe { std::str::from_utf8_unchecked(&data[i..i + valid]) }, out);
                    i += valid;
                    match e.error_len() {
                        Some(n) => {
                            self.put("\u{FFFD}", out);
                            i += n;
                        }
                        None => {
                            self.pending.extend_from_slice(&data[i..]);
                            return;
                        }
                    }
                }
            }
        }
    }

    fn put(&self, s: &str, out: &mut Vec<u8>) {
        out.reserve(s.len() * 2);
        for u in s.encode_utf16() {
            out.extend_from_slice(&if self.big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
        }
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.put("\u{FFFD}", out);
        }
    }
}

/// Converts text in the system code page to UTF-8, streaming (a double-byte character split across chunks waits).
pub struct AnsiDecoder {
    cp: u32,
    /// Lead bytes of a double-byte code page (Japanese, Chinese, Korean); all false for single-byte ones.
    lead: [bool; 256],
    dbcs: bool,
    sbcs: Option<Arc<SingleByte>>,
    pending: Option<u8>,
}

impl Default for AnsiDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiDecoder {
    pub fn new() -> Self {
        Self::with_codepage(ansi_codepage())
    }

    fn with_codepage(cp: u32) -> Self {
        #[allow(unused_mut)]
        let mut lead = [false; 256];
        #[allow(unused_mut)]
        let mut dbcs = false;
        #[cfg(windows)]
        let mut info = CPINFO::default();
        #[cfg(windows)]
        if unsafe { GetCPInfo(cp, &mut info) }.is_ok() && info.MaxCharSize > 1 {
            dbcs = true;
            for r in info.LeadByte.chunks(2) {
                if r[0] == 0 && r[1] == 0 {
                    break;
                }
                for b in r[0]..=r[1] {
                    lead[b as usize] = true;
                }
            }
            if !lead.iter().any(|&l| l) {
                // No ranges reported: fall back to asking Windows per byte value, once.
                for (b, l) in lead.iter_mut().enumerate() {
                    *l = unsafe { IsDBCSLeadByteEx(cp, b as u8) }.is_ok();
                }
            }
        }
        let sbcs = if dbcs { None } else { single_byte(cp) };
        AnsiDecoder { cp, lead, dbcs, sbcs, pending: None }
    }

    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        if !self.dbcs {
            match &self.sbcs {
                Some(t) => t.decode(input, out),
                None => out.extend_from_slice(&ansi_to_utf8_cp(input, self.cp)),
            }
            return;
        }
        let mut data = Vec::with_capacity(input.len() + 1);
        if let Some(b) = self.pending.take() {
            data.push(b);
        }
        data.extend_from_slice(input);
        if data.is_empty() {
            return;
        }
        // Don't split a double-byte character: find whether the last byte is an unfinished lead byte.
        let mut end = data.len();
        let mut i = 0;
        while i < data.len() {
            if self.lead[data[i] as usize] {
                if i + 1 >= data.len() {
                    end = i;
                    break;
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        if end < data.len() {
            self.pending = Some(data[end]);
        }
        out.extend_from_slice(&ansi_to_utf8_cp(&data[..end], self.cp));
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if let Some(b) = self.pending.take() {
            out.extend_from_slice(&ansi_to_utf8_cp(&[b], self.cp));
        }
    }
}

fn ansi_to_utf8_cp(bytes: &[u8], cp: u32) -> Vec<u8> {
    if bytes.is_empty() {
        return Vec::new();
    }
    if bytes.is_ascii() {
        return bytes.to_vec();
    }
    #[cfg(not(windows))]
    return {
        let mut out = Vec::new();
        match single_byte(cp) {
            Some(t) => t.decode(bytes, &mut out),
            None => out.extend_from_slice(String::from_utf8_lossy(bytes).as_bytes()),
        }
        out
    };
    #[cfg(windows)]
    return ansi_to_utf8_windows(bytes, cp);
}

#[cfg(windows)]
fn ansi_to_utf8_windows(bytes: &[u8], cp: u32) -> Vec<u8> {
    let n = unsafe { MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
    let mut wide = vec![0u16; n.max(0) as usize];
    unsafe { MultiByteToWideChar(cp, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide)) };
    String::from_utf16_lossy(&wide).into_bytes()
}

/// Converts UTF-8 text to the system code page. Returns the bytes and whether some characters couldn't be
/// represented (they become `?`).
pub fn utf8_to_ansi(text: &[u8]) -> (Vec<u8>, bool) {
    utf8_to_cp(text, ansi_codepage())
}

fn utf8_to_cp(text: &[u8], cp: u32) -> (Vec<u8>, bool) {
    if text.is_ascii() {
        return (text.to_vec(), false);
    }
    #[cfg(not(windows))]
    return {
        let mut out = Vec::new();
        let lossy = match single_byte(cp) {
            Some(t) => t.encode(text, &mut out),
            None => {
                out.extend(text.iter().map(|&b| if b.is_ascii() { b } else { b'?' }));
                true
            }
        };
        (out, lossy)
    };
    #[cfg(windows)]
    return utf8_to_cp_windows(text, cp);
}

#[cfg(windows)]
fn utf8_to_cp_windows(text: &[u8], cp: u32) -> (Vec<u8>, bool) {
    let wide: Vec<u16> = String::from_utf8_lossy(text).encode_utf16().collect();
    let mut lossy = windows::Win32::Foundation::BOOL(0);
    let n = unsafe {
        WideCharToMultiByte(cp, WC_NO_BEST_FIT_CHARS, &wide, None, PCSTR::null(), Some(&mut lossy as *mut _))
    };
    let mut out = vec![0u8; n.max(0) as usize];
    unsafe { WideCharToMultiByte(cp, WC_NO_BEST_FIT_CHARS, &wide, Some(&mut out), PCSTR::null(), None) };
    (out, lossy.as_bool())
}

/// A single-byte code page (Windows-1252 and the like) as tables, made once from what Windows converts each byte
/// to and back, so converting needn't ask it for every piece of text: the same result, many times faster.
struct SingleByte {
    /// Each byte as UTF-8 (its length in the last of the four).
    utf8: [[u8; 4]; 256],
    /// The characters bytes read as (sorted), with the byte each is written as again.
    back: Vec<(char, u8)>,
    /// The byte reads as a character that's written as that very byte again.
    same: [bool; 256],
    /// What a character the code page hasn't got is written as.
    default: u8,
}

/// The tables of code page `cp`, if it's a single-byte one (made once per code page). Outside Windows only
/// Windows-1252 is there, from `CP1252_HIGH`.
fn single_byte(cp: u32) -> Option<Arc<SingleByte>> {
    static TABLES: Mutex<Vec<(u32, Option<Arc<SingleByte>>)>> = Mutex::new(Vec::new());
    let mut tables = TABLES.lock().unwrap();
    if let Some((_, t)) = tables.iter().find(|t| t.0 == cp) {
        return t.clone();
    }
    #[cfg(not(windows))]
    let t = (cp == 1252).then(|| Arc::new(SingleByte::from_chars(cp1252_char, b'?')));
    #[cfg(windows)]
    let t = single_byte_windows(cp);
    tables.push((cp, t.clone()));
    t
}

/// Windows-1252's bytes 0x80 to 0x9F as Windows reads them (the five it has no character for read as the C1
/// controls of the same value, and are written as those bytes again); the others read as the Latin-1 character of
/// the same value.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D,
    0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
    0x0153, 0x009D, 0x017E, 0x0178,
];

#[cfg_attr(windows, allow(dead_code))]
fn cp1252_char(b: u8) -> char {
    match b {
        0x80..=0x9F => char::from_u32(CP1252_HIGH[(b - 0x80) as usize] as u32).unwrap(),
        _ => b as char,
    }
}

#[cfg(windows)]
fn single_byte_windows(cp: u32) -> Option<Arc<SingleByte>> {
    let mut info = CPINFO::default();
    let t = (unsafe { GetCPInfo(cp, &mut info) }.is_ok() && info.MaxCharSize == 1).then(|| {
        let mut t = SingleByte { utf8: [[0; 4]; 256], back: Vec::new(), same: [false; 256], default: info.DefaultChar[0] };
        for b in 0..=255u8 {
            let s = ansi_to_utf8_cp(&[b], cp);
            let n = s.len().min(3);
            t.utf8[b as usize][..n].copy_from_slice(&s[..n]);
            t.utf8[b as usize][3] = n as u8;
            let (again, lossy) = utf8_to_cp(&s, cp);
            t.same[b as usize] = !lossy && again == [b] && n == s.len();
            if let (Some(c), [a], false) = (std::str::from_utf8(&s).ok().and_then(|x| x.chars().next()), &again[..], lossy) {
                t.back.push((c, *a));
            }
        }
        t.back.sort_unstable();
        t.back.dedup_by_key(|e| e.0);
        Arc::new(t)
    });
    t
}

impl SingleByte {
    /// The tables of a code page in which byte `b` reads as `char_of(b)`, and every character that reads is written
    /// as its byte again (characters it hasn't got as `default`).
    #[cfg_attr(windows, allow(dead_code))]
    fn from_chars(char_of: fn(u8) -> char, default: u8) -> SingleByte {
        let mut t = SingleByte { utf8: [[0; 4]; 256], back: Vec::new(), same: [true; 256], default };
        for b in 0..=255u8 {
            let c = char_of(b);
            let n = c.encode_utf8(&mut t.utf8[b as usize][..3]).len();
            t.utf8[b as usize][3] = n as u8;
            t.back.push((c, b));
        }
        t.back.sort_unstable();
        t.back.dedup_by_key(|e| e.0);
        t
    }

    fn decode(&self, bytes: &[u8], out: &mut Vec<u8>) {
        out.reserve(bytes.len());
        let mut rest = bytes;
        while !rest.is_empty() {
            // (most text is ASCII: a run at a time)
            let k = rest.iter().position(|&b| b >= 0x80).unwrap_or(rest.len());
            out.extend_from_slice(&rest[..k]);
            let Some(&b) = rest.get(k) else { break };
            let e = &self.utf8[b as usize];
            out.extend_from_slice(&e[..e[3] as usize]);
            rest = &rest[k + 1..];
        }
    }

    /// Converts UTF-8 text (bytes that aren't UTF-8 read as U+FFFD, as for Windows); returns whether a character
    /// had to become `default`.
    fn encode(&self, text: &[u8], out: &mut Vec<u8>) -> bool {
        let mut lossy = false;
        let mut put = |c: char, out: &mut Vec<u8>| match self.back.binary_search_by_key(&c, |e| e.0) {
            Ok(i) => out.push(self.back[i].1),
            Err(_) => {
                // (one for each half of a character outside the BMP, as Windows writes them)
                out.extend(std::iter::repeat_n(self.default, c.len_utf16()));
                lossy = true;
            }
        };
        out.reserve(text.len());
        for chunk in text.utf8_chunks() {
            let mut rest = chunk.valid();
            while !rest.is_empty() {
                let k = rest.bytes().position(|b| !b.is_ascii()).unwrap_or(rest.len());
                out.extend_from_slice(&rest.as_bytes()[..k]);
                let Some(c) = rest[k..].chars().next() else { break };
                put(c, out);
                rest = &rest[k + c.len_utf8()..];
            }
            if !chunk.invalid().is_empty() {
                put('\u{FFFD}', out);
            }
        }
        lossy
    }
}

/// Splits UTF-8 into chunks at character boundaries for the ANSI encoder (streaming).
pub struct AnsiEncoder {
    cp: u32,
    sbcs: Option<Arc<SingleByte>>,
    pending: Vec<u8>,
    pub lossy: bool,
}

impl Default for AnsiEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiEncoder {
    pub fn new() -> Self {
        Self::with_codepage(ansi_codepage())
    }

    fn with_codepage(cp: u32) -> Self {
        AnsiEncoder { cp, sbcs: single_byte(cp), pending: Vec::new(), lossy: false }
    }

    fn encode(&mut self, text: &[u8], out: &mut Vec<u8>) {
        match &self.sbcs {
            Some(t) => self.lossy |= t.encode(text, out),
            None => {
                let (bytes, lossy) = utf8_to_cp(text, self.cp);
                self.lossy |= lossy;
                out.extend_from_slice(&bytes);
            }
        }
    }

    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let joined;
        let data: &[u8] = if self.pending.is_empty() {
            input
        } else {
            self.pending.extend_from_slice(input);
            joined = std::mem::take(&mut self.pending);
            &joined
        };
        // Keep an incomplete trailing sequence for the next chunk.
        let mut cut = data.len();
        for i in (data.len().saturating_sub(3)..data.len()).rev() {
            let b = data[i];
            if b & 0xC0 != 0x80 {
                if i + utf8_len(b) > data.len() {
                    cut = i;
                }
                break;
            }
        }
        self.encode(&data[..cut], out);
        self.pending = data[cut..].to_vec();
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        let rest = std::mem::take(&mut self.pending);
        self.encode(&rest, out);
    }
}

/// Length of the UTF-8 sequence a lead byte starts (1 for invalid bytes).
pub fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

pub fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Length of the valid UTF-8 character starting at `bytes[0]`, or 1 if it isn't one.
pub fn char_len_at(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let n = utf8_len(bytes[0]);
    if n == 1 || bytes.len() < n {
        return 1;
    }
    match std::str::from_utf8(&bytes[..n]) {
        Ok(_) => n,
        Err(_) => 1,
    }
}

/// Decodes one character at the start of `bytes` (invalid bytes decode as U+FFFD, length 1).
pub fn decode_char(bytes: &[u8]) -> (char, usize) {
    let n = char_len_at(bytes);
    if n == 0 {
        return ('\0', 0);
    }
    match std::str::from_utf8(&bytes[..n]) {
        Ok(s) => (s.chars().next().unwrap_or('\u{FFFD}'), n),
        Err(_) => ('\u{FFFD}', 1),
    }
}

/// Decodes the character ending at `bytes.len()` (looking back at most 4 bytes).
pub fn decode_char_before(bytes: &[u8]) -> (char, usize) {
    let len = bytes.len();
    if len == 0 {
        return ('\0', 0);
    }
    for back in 2..=4.min(len) {
        let s = len - back;
        if !is_continuation(bytes[s]) {
            if char_len_at(&bytes[s..]) == back {
                return decode_char(&bytes[s..]);
            }
            break;
        }
    }
    decode_char(&bytes[len - 1..])
}

/// Visible stand-in for characters that would otherwise be invisible or break the layout.
fn display_char(c: char) -> char {
    match c {
        '\t' => '\t',
        '\u{0}'..='\u{1F}' => char::from_u32(0x2400 + c as u32).unwrap(),
        '\u{7F}' => '\u{2421}',
        '\u{85}' | '\u{2028}' | '\u{2029}' => '\u{2424}',
        _ => c,
    }
}

/// Decodes UTF-8 for display. Invalid bytes become U+FFFD (one per byte) and control characters visible symbols.
/// `map[i]` is the byte offset of UTF-16 unit `i`; `map` gets one extra entry for the end.
pub fn decode_display(bytes: &[u8], out: &mut Vec<u16>, map: &mut Vec<u32>) {
    fn push_str(s: &str, base: usize, out: &mut Vec<u16>, map: &mut Vec<u32>) {
        for (k, c) in s.char_indices() {
            let c = display_char(c);
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push(*u);
                map.push((base + k) as u32);
            }
        }
    }
    out.clear();
    map.clear();
    let mut i = 0;
    while i < bytes.len() {
        match std::str::from_utf8(&bytes[i..]) {
            Ok(s) => {
                push_str(s, i, out, map);
                break;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                push_str(unsafe { std::str::from_utf8_unchecked(&bytes[i..i + valid]) }, i, out, map);
                i += valid;
                let bad = e.error_len().unwrap_or(bytes.len() - i);
                for k in 0..bad {
                    out.push(0xFFFD);
                    map.push((i + k) as u32);
                }
                i += bad;
            }
        }
    }
    map.push(bytes.len() as u32);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharClass {
    Space,
    Word,
    Punct,
    Newline,
}

pub fn char_class(c: char) -> CharClass {
    if c == '\n' || c == '\r' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

// ---- characters as people see them (a small part of Unicode's grapheme clusters, UAX #29) ----

/// Marks that belong to the character before them: combining accents (the common blocks, Hebrew and Arabic points,
/// the vowel signs of Indic scripts, Thai and Lao), variation selectors, emoji skin tones, the keycap mark, emoji
/// tags and the zero-width non-joiner.
fn is_extend(u: u32) -> bool {
    match u {
        // Devanagari … Malayalam share one layout: signs, nukta, vowel signs and virama, length marks
        0x0900..=0x0D7F => matches!(u & 0x7F, 0x00..=0x03 | 0x3A..=0x3C | 0x3E..=0x4F | 0x51..=0x57 | 0x62..=0x63),
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7 => true,
        0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 => true,
        0x06EA..=0x06ED | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x0EB1 | 0x0EB4..=0x0EBC | 0x0EC8..=0x0ECD => true,
        0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200C | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F => true,
        0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF => true,
        _ => false,
    }
}

fn is_regional(u: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&u)
}

/// A virama that joins the next consonant into a conjunct (क्ष is one character): Devanagari, Bengali, Gujarati,
/// Oriya, Telugu, Malayalam.
fn is_linker(u: u32) -> bool {
    matches!(u, 0x094D | 0x09CD | 0x0ACD | 0x0B4D | 0x0C4D | 0x0D4D)
}

/// Length in bytes of the character as people see it at the start of `bytes`: a letter with its accents, an
/// emoji with its skin tone or the ZWJ sequence it starts (👨‍👩‍👧), a flag (two regional letters), a keycap.
/// Line breaks and invalid bytes stand alone.
pub fn cluster_len_at(bytes: &[u8]) -> usize {
    let (first, mut n) = decode_char(bytes);
    if n == 0 || first == '\r' || first == '\n' || std::str::from_utf8(&bytes[..n]).is_err() {
        return n;
    }
    let mut prev = first as u32;
    let mut regional = is_regional(prev) as u32;
    while n < bytes.len() {
        let (c, len) = decode_char(&bytes[n..]);
        let u = c as u32;
        let joins = is_extend(u)
            || u == 0x200D
            // after a zero-width joiner, the next character is part of the sequence
            || (prev == 0x200D && !c.is_control())
            // a conjunct: the consonant after a virama, in the same script
            || (is_linker(prev) && u >> 7 == prev >> 7)
            // flags: regional letters go in pairs
            || (is_regional(u) && regional % 2 == 1);
        if !joins || std::str::from_utf8(&bytes[n..n + len]).is_err() {
            break;
        }
        if is_regional(u) {
            regional += 1;
        }
        prev = u;
        n += len;
    }
    n
}

/// Length in bytes of the visible character that ends at the end of `bytes`; `bytes` should start where a
/// character starts (at a line start, say), as it is worked out from there.
pub fn cluster_len_before(bytes: &[u8]) -> usize {
    let mut at = 0;
    let mut last = 0;
    while at < bytes.len() {
        last = cluster_len_at(&bytes[at..]).max(1);
        at += last;
    }
    // (a window that started in the middle of a character: what's left of it)
    last.min(bytes.len())
}

/// What Backspace deletes before the caret: an emoji sequence or flag as a whole, but of a letter with accents
/// only its last accent (as Windows does), and otherwise one character.
pub fn backspace_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let n = cluster_len_before(bytes);
    let cluster = &bytes[bytes.len() - n..];
    let emoji = std::str::from_utf8(cluster).is_ok_and(|s| {
        s.chars().any(|c| matches!(c as u32, 0x200D | 0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F) || is_regional(c as u32))
    });
    if emoji { n } else { decode_char_before(bytes).1.max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_characters() {
        let at = |s: &str| cluster_len_at(s.as_bytes());
        let before = |s: &str| cluster_len_before(s.as_bytes());
        assert_eq!(at("e\u{301}x"), 3); // e + combining acute
        assert_eq!(at("\u{1F44D}\u{1F3FD}!"), 8); // 👍🏽
        assert_eq!(at("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}y"), 18); // 👨‍👩‍👧
        assert_eq!(at("\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}"), 8); // 🇩🇪 then 🇫🇷
        assert_eq!(at("1\u{FE0F}\u{20E3}"), 7); // keycap 1️⃣
        assert_eq!(at("\u{2764}\u{FE0F}"), 6); // ❤️
        assert_eq!(at("\u{915}\u{94D}\u{937}\u{93F}x"), 12); // क्षि: a conjunct with its vowel sign
        assert_eq!(at("\u{915}\u{93F}\u{915}"), 6); // कि then क
        assert_eq!(at("\u{B95}\u{BCD}\u{BB7}"), 6); // Tamil's pulli shows: க் then ஷ
        assert_eq!(at("ab"), 1);
        assert_eq!(at("\r\n"), 1);
        assert_eq!(at("\u{301}a"), 2); // a stray accent goes with nothing before it
        assert_eq!(cluster_len_at(b"\xFFa"), 1);
        assert_eq!(before("x\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}"), 8);
        assert_eq!(before("xe\u{301}"), 3);
        assert_eq!(before("ab"), 1);
        assert_eq!(before(""), 0);
        // Backspace: the whole emoji or flag, but only the last accent of a letter
        assert_eq!(backspace_len("x\u{1F44D}\u{1F3FD}".as_bytes()), 8);
        assert_eq!(backspace_len("x\u{1F1E9}\u{1F1EA}".as_bytes()), 8);
        assert_eq!(backspace_len("xe\u{301}".as_bytes()), 2);
        assert_eq!(backspace_len(b"ab"), 1);
    }

    #[test]
    fn detects_encodings() {
        assert_eq!(detect_encoding(b"\xEF\xBB\xBFhi", false), (Encoding::Utf8Bom, 3));
        assert_eq!(detect_encoding(b"\xFF\xFEh\0i\0", false), (Encoding::Utf16Le, 2));
        assert_eq!(detect_encoding("hello caf\u{e9}".as_bytes(), false), (Encoding::Utf8, 0));
        assert_eq!(detect_encoding(b"hello caf\xE9 au lait", false), (Encoding::Ansi, 0));
        // cut in the middle of a character at the end of a sample
        assert_eq!(detect_encoding(b"abc\xE2\x82", true), (Encoding::Utf8, 0));
        let le: Vec<u8> = "plain text without a bom".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(detect_encoding(&le, false), (Encoding::Utf16Le, 0));
        let be: Vec<u8> = "plain text without a bom".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(detect_encoding(&be, false), (Encoding::Utf16Be, 0));
        assert_eq!(detect_eol(b"a\r\nb\r\nc\n"), Eol::Crlf);
        assert_eq!(detect_eol(b"a\nb\nc\r\n"), Eol::Lf);
        assert_eq!(detect_eol(b"abc"), Eol::NATIVE);
    }

    #[test]
    fn ansi_only_when_it_converts_back() {
        // UTF-8 with one bad byte stays UTF-8; text with only Windows-1252 accents is ANSI
        let mut damaged = "Grüße – naïve café ✓ 日本語\r\n".repeat(10).into_bytes();
        damaged.push(0xFF);
        assert_eq!(detect_encoding(&damaged, false), (Encoding::Utf8, 0));
        let latin1 = b"caf\xE9 cr\xE8me br\xFBl\xE9e\r\n".repeat(10);
        assert_eq!(detect_encoding(&latin1, false), (Encoding::Ansi, 0));
        // every Windows-1252 byte converts back to itself...
        let all: Vec<u8> = (0..=255).collect();
        assert!(fits_cp(&all, false, 1252));
        assert!(fits_cp(&latin1, false, 1252));
        // ...but in a double-byte code page (Japanese) not every byte sequence is text: not taken as ANSI there
        #[cfg(windows)]
        {
            let mut not_sjis = "naïve café ✓\r\n".repeat(10).into_bytes();
            not_sjis.push(0xFF);
            assert!(!fits_cp(&not_sjis, false, 932));
            let (sjis, lossy) = utf8_to_cp("日本語のテキスト\r\n".repeat(10).as_bytes(), 932);
            assert!(!lossy && fits_cp(&sjis, false, 932));
            // (cut in the middle of a double-byte character at the end of a sample: fine)
            assert!(fits_cp(&sjis[..sjis.len() - 3], true, 932));
            // the "UTF-8 for worldwide language support" option: ANSI means Windows-1252 then
            assert_eq!(effective_codepage(65001), 1252);
            assert_eq!(effective_codepage(932), 932);
        }
        // the streaming check used while converting big files, fed in pieces
        if ansi_codepage() == 1252 {
            let mut check = AnsiCheck::new();
            let mut dec = AnsiDecoder::new();
            for part in latin1.chunks(7) {
                let mut text = Vec::new();
                dec.push(part, &mut text);
                check.push(part, &text);
            }
            let mut text = Vec::new();
            dec.finish(&mut text);
            assert!(check.finish(&text));
        }
    }

    #[test]
    fn single_byte_tables_convert_as_windows_does() {
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        // Western, Central European, Cyrillic, Greek (with bytes it hasn't got), Hebrew, Thai
        for cp in [1252, 1250, 1251, 1253, 1255, 874] {
            let Some(t) = single_byte(cp) else { continue };
            let all: Vec<u8> = (0..=255).collect();
            let mut text = Vec::new();
            t.decode(&all, &mut text);
            assert_eq!(text, ansi_to_utf8_cp(&all, cp), "code page {cp}");
            // random bytes: decoded, and whether they come back
            for _ in 0..50 {
                let bytes: Vec<u8> = (0..next() % 300).map(|_| next() as u8).collect();
                let mut text = Vec::new();
                t.decode(&bytes, &mut text);
                assert_eq!(text, ansi_to_utf8_cp(&bytes, cp));
                let (back, lossy) = utf8_to_cp(&text, cp);
                assert_eq!(round_trips_cp(&text, &bytes, cp), !lossy && back == bytes, "code page {cp}");
            }
            // random text (with characters it hasn't got, and bytes that aren't UTF-8): encoded
            let parts: [&[u8]; 9] = [b"plain ", "café".as_bytes(), "€ – “q”".as_bytes(), "Ωμέγα".as_bytes(), "Жж".as_bytes(), "שלום".as_bytes(), "ไทย".as_bytes(), "🌍".as_bytes(), b"\xFF\xE2\x82"];
            for _ in 0..200 {
                let text: Vec<u8> = (0..next() % 8).flat_map(|_| parts[(next() % 9) as usize].to_vec()).collect();
                let mut out = Vec::new();
                let lossy = t.encode(&text, &mut out);
                assert_eq!((out, lossy), utf8_to_cp(&text, cp), "code page {cp}: {:?}", String::from_utf8_lossy(&text));
            }
        }
        // the Windows-1252 table used where Windows can't be asked (Linux) is the one Windows makes
        #[cfg(windows)]
        {
            let (ours, windows) = (SingleByte::from_chars(cp1252_char, b'?'), single_byte(1252).unwrap());
            assert_eq!((ours.utf8, ours.same, ours.default), (windows.utf8, windows.same, windows.default));
            assert_eq!(ours.back, windows.back);
        }
        // the streaming encoder: a character cut between pieces waits for the rest
        let mut enc = AnsiEncoder::with_codepage(1252);
        let mut out = Vec::new();
        for piece in "café €".as_bytes().chunks(1) {
            enc.push(piece, &mut out);
        }
        enc.finish(&mut out);
        assert_eq!((out, enc.lossy), (b"caf\xE9 \x80".to_vec(), false));
    }

    #[test]
    fn utf16_round_trip_streaming() {
        let text = "Grüße 🌍 — ok\r\nline2";
        for be in [false, true] {
            let mut bytes = Vec::new();
            let mut enc = Utf16Encoder::new(be);
            for chunk in text.as_bytes().chunks(3) {
                enc.push(chunk, &mut bytes);
            }
            enc.finish(&mut bytes);
            let mut back = Vec::new();
            let mut dec = Utf16Decoder::new(be);
            for chunk in bytes.chunks(3) {
                dec.push(chunk, &mut back);
            }
            dec.finish(&mut back);
            assert_eq!(String::from_utf8(back).unwrap(), text);
        }
    }

    #[test]
    fn ansi_round_trip() {
        let mut dec = AnsiDecoder::new();
        let mut out = Vec::new();
        dec.push(b"caf\xE9 \x80", &mut out);
        dec.finish(&mut out);
        if ansi_codepage() == 1252 {
            assert_eq!(String::from_utf8(out.clone()).unwrap(), "café €");
        }
        let mut enc = AnsiEncoder::new();
        let mut back = Vec::new();
        for chunk in out.chunks(1) {
            enc.push(chunk, &mut back);
        }
        enc.finish(&mut back);
        assert_eq!(back, b"caf\xE9 \x80");
        assert!(!enc.lossy);
        let (_, lossy) = utf8_to_ansi("emoji 🌍".as_bytes());
        assert!(lossy);
    }

    #[test]
    fn display_decoding_maps_bytes() {
        let mut out = Vec::new();
        let mut map = Vec::new();
        decode_display(b"a\tb\x01\xFFc\xF0\x9F\x8C\x8D", &mut out, &mut map);
        assert_eq!(out[0], 'a' as u16);
        assert_eq!(out[1], '\t' as u16);
        assert_eq!(out[3], 0x2401);
        assert_eq!(out[4], 0xFFFD);
        assert_eq!(map[4], 4);
        assert_eq!(map[5], 5);
        assert_eq!(map[6], 6); // high surrogate of the emoji
        assert_eq!(map[7], 6); // low surrogate maps to the same byte
        assert_eq!(*map.last().unwrap(), 10);
    }

    #[test]
    fn char_helpers() {
        let s = "aé🌍".as_bytes();
        assert_eq!(char_len_at(&s[1..]), 2);
        assert_eq!(char_len_at(&s[3..]), 4);
        assert_eq!(decode_char_before(s), ('🌍', 4));
        assert_eq!(decode_char_before(&s[..3]), ('é', 2));
        assert_eq!(decode_char_before(b"\xFF"), ('\u{FFFD}', 1));
        assert_eq!(char_len_at(b"\xE2\x82"), 1);
    }
}
