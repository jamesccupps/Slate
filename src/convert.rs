//! Format → Convert (both apps): Base64, URL encoding and JSON string escapes, each way, and SHA-256 — on the
//! selection, or on the whole text when nothing is selected. Encoding takes any bytes; decoding says what's wrong
//! with its input instead of guessing.

/// Format → Convert's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    Base64Encode,
    Base64Decode,
    UrlEncode,
    UrlDecode,
    JsonEscape,
    JsonUnescape,
    /// Shown (and copied if asked), not put in the text.
    Sha256,
}

/// Up to this much text is converted at once (the selection, or the whole text when nothing is selected); SHA-256
/// takes any size (on another thread).
pub const CONVERT_MAX: u64 = 16 << 20;

impl Conv {
    pub const ALL: [Conv; 7] =
        [Conv::Base64Encode, Conv::Base64Decode, Conv::UrlEncode, Conv::UrlDecode, Conv::JsonEscape, Conv::JsonUnescape, Conv::Sha256];

    /// The menu's label.
    pub fn label(self) -> &'static str {
        match self {
            Conv::Base64Encode => "Base64 encode",
            Conv::Base64Decode => "Base64 decode",
            Conv::UrlEncode => "URL encode",
            Conv::UrlDecode => "URL decode",
            Conv::JsonEscape => "JSON string escape",
            Conv::JsonUnescape => "JSON string unescape",
            Conv::Sha256 => "SHA-256",
        }
    }

    /// A name for scripts and menus' actions (`base64-encode`).
    pub fn key(self) -> &'static str {
        match self {
            Conv::Base64Encode => "base64-encode",
            Conv::Base64Decode => "base64-decode",
            Conv::UrlEncode => "url-encode",
            Conv::UrlDecode => "url-decode",
            Conv::JsonEscape => "json-escape",
            Conv::JsonUnescape => "json-unescape",
            Conv::Sha256 => "sha256",
        }
    }

    /// What it makes of `text` (not SHA-256's: that's `Sha256`), or why it can't.
    pub fn apply(self, text: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Conv::Base64Encode => Ok(base64_encode(text)),
            Conv::Base64Decode => base64_decode(text),
            Conv::UrlEncode => Ok(url_encode(text).into_bytes()),
            Conv::UrlDecode => url_decode(text),
            Conv::JsonEscape => Ok(json_escape(text)),
            Conv::JsonUnescape => json_unescape(text),
            Conv::Sha256 => Err("SHA-256 isn't put in the text".into()),
        }
    }

    /// What the status bar says once it's done (`out` the result's length).
    pub fn done(self, out: u64) -> String {
        let what = match self {
            Conv::Base64Encode => "Base64 encoded",
            Conv::Base64Decode => "Base64 decoded",
            Conv::UrlEncode => "URL encoded",
            Conv::UrlDecode => "URL decoded",
            Conv::JsonEscape => "Escaped for a JSON string",
            Conv::JsonUnescape => "JSON escapes undone",
            Conv::Sha256 => "SHA-256",
        };
        format!("{what} ({})", crate::edit::plural(out, "byte", "bytes"))
    }
}

/// The SHA-256 of `snap`'s bytes `from..to`, a megabyte at a time (None: cancelled).
pub fn sha256_of(snap: &crate::core::buffer::Snapshot, from: u64, to: u64, ctx: &crate::core::job::Ctx) -> Option<[u8; 32]> {
    let mut h = Sha256::default();
    let mut pos = from;
    while pos < to {
        if ctx.cancelled() {
            return None;
        }
        let end = (pos + (1 << 20)).min(to);
        snap.chunks(pos, end, &mut |c| {
            h.update(c);
            true
        });
        ctx.set(end - from);
        pos = end;
    }
    Some(h.finish())
}

/// Base64 of `b` (the standard alphabet, padded with `=`).
pub fn base64_encode(b: &[u8]) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63]);
        out.push(A[(n >> 12) as usize & 63]);
        out.push(if c.len() > 1 { A[(n >> 6) as usize & 63] } else { b'=' });
        out.push(if c.len() > 2 { A[n as usize & 63] } else { b'=' });
    }
    out
}

/// The bytes Base64 text stands for: the standard alphabet or the URL one (`-`, `_`), padding or none; spaces and
/// line breaks are skipped (Base64 is often wrapped).
pub fn base64_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for (i, &c) in b.iter().enumerate() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(format!("That isn't Base64: \"{}\" at character {}", show(&b[i..]), group(i as u64 + 1))),
        };
        if pad > 0 {
            return Err("That isn't Base64: it goes on after its \"=\" padding".into());
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // (a lone character left over is 6 bits: no byte)
    if bits == 6 || pad > 2 {
        return Err("That isn't Base64: its length is off by a character".into());
    }
    Ok(out)
}

/// `b` percent-encoded for a URL's part (a query value, a path segment): letters, digits and `-_.~` as they are,
/// every other byte as `%XX`.
pub fn url_encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 3);
    for &c in b {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// The bytes percent-encoded text stands for (`%XX` each; `+` stays `+`, as in a path: in a form's query it
/// means a space, which `decodeURIComponent` doesn't say either).
pub fn url_decode(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
            match hex {
                Some(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                None => return Err(format!("That isn't URL encoding: \"{}\" at character {}", show(&b[i..]), group(i as u64 + 1))),
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Ok(out)
}

/// `b` as the inside of a JSON string: `"` and `\` escaped, line breaks and tabs as `\n`, `\r`, `\t`, the other
/// control characters as `\u00XX`; everything else as it is (UTF-8 stays UTF-8).
pub fn json_escape(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() + b.len() / 8);
    for &c in b {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            0..0x20 | 0x7F => out.extend_from_slice(format!("\\u{c:04x}").as_bytes()),
            _ => out.push(c),
        }
    }
    out
}

/// The text a JSON string's escapes stand for (with its quotes, or just what's between them); `\uXXXX` pairs make
/// the characters past U+FFFF.
pub fn json_unescape(b: &[u8]) -> Result<Vec<u8>, String> {
    let b = match b {
        [b'"', inner @ .., b'"'] => inner,
        _ => b,
    };
    let mut out = Vec::with_capacity(b.len());
    let bad = |i: usize| format!("That isn't a JSON string: \"{}\" at character {}", show(&b[i..]), group(i as u64 + 1));
    let hex4 = |i: usize| b.get(i..i + 4).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u32::from_str_radix(h, 16).ok());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let Some(&e) = b.get(i + 1) else { return Err(bad(i)) };
        let simple = match e {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'b' => Some(0x08),
            b'f' => Some(0x0C),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            i += 2;
            continue;
        }
        if e != b'u' {
            return Err(bad(i));
        }
        let Some(hi) = hex4(i + 2) else { return Err(bad(i)) };
        let mut n = 6;
        let cp = if (0xD800..0xDC00).contains(&hi) {
            // a surrogate pair: the low half must follow
            match (b.get(i + 6..i + 8), hex4(i + 8)) {
                (Some(b"\\u"), Some(lo)) if (0xDC00..0xE000).contains(&lo) => {
                    n = 12;
                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                }
                _ => return Err(bad(i)),
            }
        } else {
            hi
        };
        let Some(c) = char::from_u32(cp) else { return Err(bad(i)) };
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        i += n;
    }
    Ok(out)
}

/// A few characters of `b` from where something's wrong, for a message.
fn show(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(&b[..b.len().min(8)]).into_owned();
    s.chars().take(6).map(|c| if c.is_control() { '\u{FFFD}' } else { c }).collect()
}

fn group(n: u64) -> String {
    crate::edit::group(n)
}

/// SHA-256, fed in pieces (a whole file's is worked out on another thread, a megabyte at a time).
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    used: usize,
    len: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            used: 0,
            len: 0,
        }
    }
}

impl Sha256 {
    pub fn update(&mut self, mut b: &[u8]) {
        self.len += b.len() as u64;
        if self.used > 0 {
            let n = (64 - self.used).min(b.len());
            self.buf[self.used..self.used + n].copy_from_slice(&b[..n]);
            self.used += n;
            b = &b[n..];
            if self.used < 64 {
                return;
            }
            let block = self.buf;
            self.block(&block);
            self.used = 0;
        }
        let mut blocks = b.chunks_exact(64);
        for block in &mut blocks {
            self.block(block.try_into().unwrap());
        }
        let rest = blocks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.used = rest.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        let mut tail = vec![0x80u8];
        tail.resize((64 + 56 - (self.used + 1) % 64) % 64 + 1, 0);
        tail.extend_from_slice(&bits.to_be_bytes());
        let len = self.len;
        self.update(&tail);
        self.len = len;
        let mut out = [0u8; 32];
        for (o, h) in out.chunks_exact_mut(4).zip(self.h) {
            o.copy_from_slice(&h.to_be_bytes());
        }
        out
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(c.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (s, v) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// A SHA-256 as 64 hex digits (as `sha256sum` prints it).
pub fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_both_ways() {
        for (plain, coded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64_encode(plain.as_bytes()), coded.as_bytes());
            assert_eq!(base64_decode(coded.as_bytes()).unwrap(), plain.as_bytes());
        }
        // URL alphabet, no padding, wrapped
        assert_eq!(base64_decode(b"-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64_decode(b"Zm9v\r\nYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode(b"Zm9vY").unwrap_err(), "That isn't Base64: its length is off by a character");
        assert!(base64_decode(b"Zm9v!").unwrap_err().contains("\"!\" at character 5"));
        assert!(base64_decode(b"Zg==Zg").is_err());
    }

    #[test]
    fn url_encoding_both_ways() {
        assert_eq!(url_encode("a b&c=d/é~".as_bytes()), "a%20b%26c%3Dd%2F%C3%A9~");
        assert_eq!(url_decode(b"a%20b%26c%3Dd%2F%C3%A9~+").unwrap(), "a b&c=d/é~+".as_bytes());
        assert!(url_decode(b"100%").unwrap_err().contains("\"%\" at character 4"));
        assert!(url_decode(b"%zz").is_err());
    }

    #[test]
    fn json_escapes_both_ways() {
        let s = "say \"hi\"\\\n\ttab\u{1}é😀";
        let e = json_escape(s.as_bytes());
        assert_eq!(e, "say \\\"hi\\\"\\\\\\n\\ttab\\u0001é😀".as_bytes());
        assert_eq!(json_unescape(&e).unwrap(), s.as_bytes());
        // with its quotes; \u escapes and a surrogate pair; \/
        assert_eq!(json_unescape(br#""caf\u00e9 \ud83d\ude00 a\/b""#).unwrap(), "café 😀 a/b".as_bytes());
        assert!(json_unescape(br"bad \x").unwrap_err().contains("at character 5"));
        assert!(json_unescape(br"\ud83d alone").is_err());
        assert!(json_unescape(br"ends \").is_err());
    }

    #[test]
    fn sha256_known_values() {
        let d = |b: &[u8]| {
            let mut h = Sha256::default();
            h.update(b);
            hex(&h.finish())
        };
        assert_eq!(d(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(d(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            d(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // fed in odd pieces, across the 64-byte blocks: the same
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut h = Sha256::default();
        for c in data.chunks(13) {
            h.update(c);
        }
        assert_eq!(hex(&h.finish()), d(&data));
        let million = vec![b'a'; 1_000_000];
        assert_eq!(d(&million), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }
}
