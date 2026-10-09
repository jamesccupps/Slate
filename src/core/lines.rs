//! Line tools: sort lines, remove duplicate or blank lines, trim trailing spaces, change letter case. They work on
//! text in memory (a selection, or a whole document up to a size limit).

use std::cmp::Ordering;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineOp {
    SortAsc,
    SortDesc,
    Dedupe,
    RemoveBlank,
    TrimTrailing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseOp {
    Upper,
    Lower,
    Title,
}

/// Compares the way people sort: letters ignoring case, accented Latin letters next to their plain one (é with e,
/// accents deciding only between otherwise equal lines), numbers by value: "file2" before "file10", "-5" before "3",
/// "1.25" before "1.5". A number glued to a word or to another number's dot is a whole number, so versions and
/// addresses sort by their parts ("v1.9" before "v1.10", "1.2.3" before "1.10.0").
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    // What both start with compares equal: start where they differ, back at the start of the character or number
    // there (the byte before is then a character of its own in both, so both passes would get there together).
    let mut k = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    while k > 0 && (a[k - 1] >= 0x80 || a[k - 1].is_ascii_digit() || a[k - 1] == b'.' || a[k - 1] == b'-') {
        k -= 1;
    }
    natural(a, b, k, true).then_with(|| natural(a, b, k, false))
}

/// One pass of `natural_cmp` from `k` (in both); `base`: letters compare by their plain letter.
fn natural(a: &[u8], b: &[u8], k: usize, base: bool) -> Ordering {
    let class = |n: &Number| if n.neg { b'-' as u32 } else { b'0' as u32 };
    let (mut i, mut j) = (k, k);
    while i < a.len() && j < b.len() {
        let c = match (number_at(a, i), number_at(b, j)) {
            (Some(x), Some(y)) => {
                let c = cmp_numbers(a, &x, b, &y);
                (i, j) = (x.end, y.end);
                c
            }
            // a number and a character: by the character the number starts with ('-' or a digit)
            (Some(x), None) => class(&x).cmp(&char_key(b, j, base).0).then(Ordering::Less),
            (None, Some(y)) => char_key(a, i, base).0.cmp(&class(&y)).then(Ordering::Greater),
            (None, None) => {
                let ((x, la), (y, lb)) = (char_key(a, i, base), char_key(b, j, base));
                (i, j) = (i + la, j + lb);
                x.cmp(&y)
            }
        };
        if c != Ordering::Equal {
            return c;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// A number in a line: its sign, digits before and after the decimal point, and where it ends.
struct Number {
    neg: bool,
    int: (usize, usize),
    frac: (usize, usize),
    end: usize,
}

fn number_at(s: &[u8], i: usize) -> Option<Number> {
    let digit = |k: usize| s.get(k).is_some_and(|c| c.is_ascii_digit());
    let prev = if i > 0 { s[i - 1] } else { b' ' };
    // not glued to a word or to another number ("x-5", "v1.10", "1.2.3")
    let free = !prev.is_ascii_alphanumeric() && prev != b'.';
    let neg = s[i] == b'-' && free && prev != b'-' && digit(i + 1);
    let mut k = i + neg as usize;
    if !digit(k) {
        return None;
    }
    let from = k;
    while digit(k) {
        k += 1;
    }
    let int = (from, k);
    let mut frac = (k, k);
    if free && s.get(k) == Some(&b'.') && digit(k + 1) {
        let mut f = k + 1;
        while digit(f) {
            f += 1;
        }
        if !(s.get(f) == Some(&b'.') && digit(f + 1)) {
            frac = (k + 1, f);
            k = f;
        }
    }
    Some(Number { neg, int, frac, end: k })
}

fn cmp_numbers(a: &[u8], x: &Number, b: &[u8], y: &Number) -> Ordering {
    if x.neg != y.neg {
        return if x.neg { Ordering::Less } else { Ordering::Greater };
    }
    let (p, q) = (trim_zeros(&a[x.int.0..x.int.1]), trim_zeros(&b[y.int.0..y.int.1]));
    let mut c = p.len().cmp(&q.len()).then_with(|| p.cmp(q));
    if c == Ordering::Equal {
        // fractions digit by digit ("5" after "25"; missing digits are zeros)
        let (f, g) = (&a[x.frac.0..x.frac.1], &b[y.frac.0..y.frac.1]);
        for k in 0..f.len().max(g.len()) {
            let (d, e) = (f.get(k).copied().unwrap_or(b'0'), g.get(k).copied().unwrap_or(b'0'));
            if d != e {
                c = d.cmp(&e);
                break;
            }
        }
    }
    if x.neg { c.reverse() } else { c }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&b| b == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

/// The plain letters of U+00C0…U+017F (Latin-1 and Latin Extended-A), 8 per row; '.' = not a letter.
const LATIN: &str = concat!(
    "aaaaaaac", // À Á Â Ã Ä Å Æ Ç
    "eeeeiiii", // È É Ê Ë Ì Í Î Ï
    "dnooooo.", // Ð Ñ Ò Ó Ô Õ Ö ×
    "ouuuuyts", // Ø Ù Ú Û Ü Ý Þ ß
    "aaaaaaac", // à á â ã ä å æ ç
    "eeeeiiii", // è é ê ë ì í î ï
    "dnooooo.", // ð ñ ò ó ô õ ö ÷
    "ouuuuyty", // ø ù ú û ü ý þ ÿ
    "aaaaaacc", // Ā ā Ă ă Ą ą Ć ć
    "ccccccdd", // Ĉ ĉ Ċ ċ Č č Ď ď
    "ddeeeeee", // Đ đ Ē ē Ĕ ĕ Ė ė
    "eeeegggg", // Ę ę Ě ě Ĝ ĝ Ğ ğ
    "gggghhhh", // Ġ ġ Ģ ģ Ĥ ĥ Ħ ħ
    "iiiiiiii", // Ĩ ĩ Ī ī Ĭ ĭ Į į
    "iiiijjkk", // İ ı Ĳ ĳ Ĵ ĵ Ķ ķ
    "klllllll", // ĸ Ĺ ĺ Ļ ļ Ľ ľ Ŀ
    "lllnnnnn", // ŀ Ł ł Ń ń Ņ ņ Ň
    "nnnnoooo", // ň ŉ Ŋ ŋ Ō ō Ŏ ŏ
    "oooorrrr", // Ő ő Œ œ Ŕ ŕ Ŗ ŗ
    "rrssssss", // Ř ř Ś ś Ŝ ŝ Ş ş
    "sstttttt", // Š š Ţ ţ Ť ť Ŧ ŧ
    "uuuuuuuu", // Ũ ũ Ū ū Ŭ ŭ Ů ů
    "uuuuwwyy", // Ű ű Ų ų Ŵ ŵ Ŷ ŷ
    "yzzzzzzs", // Ÿ Ź ź Ż ż Ž ž ſ
);
const _: () = assert!(LATIN.len() == 0x180 - 0xC0);

/// The character at `i` for sorting (lowercase; with `base`, accented Latin letters as their plain letter) and its
/// length in bytes. Bytes that aren't UTF-8 come after every character.
fn char_key(s: &[u8], i: usize, base: bool) -> (u32, usize) {
    let b = s[i];
    if b < 0x80 {
        return (b.to_ascii_lowercase() as u32, 1);
    }
    let n = match b {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 0,
    };
    let c = s.get(i..i + n).filter(|_| n > 0).and_then(|x| std::str::from_utf8(x).ok()).and_then(|x| x.chars().next());
    match c {
        Some(c) => {
            let cp = c as u32;
            if base && (0xC0..0x180).contains(&cp) && LATIN.as_bytes()[(cp - 0xC0) as usize] != b'.' {
                return (LATIN.as_bytes()[(cp - 0xC0) as usize] as u32, n);
            }
            (c.to_lowercase().next().unwrap_or(c) as u32, n)
        }
        None => (0x110000 + b as u32, 1),
    }
}

/// Runs `op` over `text` (whole lines). Returns the new text and how many lines were sorted, removed or changed.
/// Line breaks come out as the kind most of the text uses.
pub fn apply(op: LineOp, text: &[u8]) -> (Vec<u8>, u64) {
    let ends_nl = text.last() == Some(&b'\n');
    let body = if ends_nl { &text[..text.len() - 1] } else { text };
    // CRLF if most line breaks are
    let crlf = memchr::memmem::find_iter(text, b"\r\n").count() * 2 > memchr::memchr_iter(b'\n', text).count();
    // Lines without their line break.
    let mut lines: Vec<&[u8]> = body.split(|&b| b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l)).collect();
    let before = lines.len() as u64;
    let mut count = 0;
    match op {
        LineOp::SortAsc | LineOp::SortDesc => {
            let was = lines.clone();
            // (lines that compare equal are the same bytes: an unstable sort gives the same result)
            lines.sort_unstable_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
            if op == LineOp::SortDesc {
                lines.reverse();
            }
            count = if lines == was { 0 } else { before };
        }
        LineOp::Dedupe => {
            let mut seen = HashSet::with_capacity(lines.len());
            lines.retain(|l| seen.insert(*l));
            count = before - lines.len() as u64;
        }
        LineOp::RemoveBlank => {
            lines.retain(|l| !l.iter().all(|b| b.is_ascii_whitespace()));
            count = before - lines.len() as u64;
        }
        LineOp::TrimTrailing => {
            for l in lines.iter_mut() {
                let t = l.trim_ascii_end();
                if t.len() != l.len() {
                    *l = t;
                    count += 1;
                }
            }
        }
    }
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let mut out = Vec::with_capacity(text.len());
    for (k, l) in lines.iter().enumerate() {
        if k > 0 {
            out.extend_from_slice(eol);
        }
        out.extend_from_slice(l);
    }
    if ends_nl && !lines.is_empty() {
        out.extend_from_slice(eol);
    }
    (out, count)
}

/// Changes the letter case of `text`. Title Case starts each word with a capital.
pub fn change_case(text: &[u8], op: CaseOp) -> Vec<u8> {
    let Ok(s) = std::str::from_utf8(text) else {
        // not valid UTF-8: only ASCII letters change
        return match op {
            CaseOp::Lower => text.to_ascii_lowercase(),
            CaseOp::Upper => text.to_ascii_uppercase(),
            CaseOp::Title => {
                let mut start = true;
                text.iter()
                    .map(|&b| {
                        let c = if b.is_ascii_alphanumeric() {
                            if start { b.to_ascii_uppercase() } else { b.to_ascii_lowercase() }
                        } else {
                            b
                        };
                        if b.is_ascii_alphanumeric() || b >= 0x80 {
                            start = false;
                        } else if b.is_ascii_whitespace() || b"-_/([{\"".contains(&b) {
                            start = true;
                        }
                        c
                    })
                    .collect()
            }
        };
    };
    // (most text is ASCII, which goes a run at a time; a selection can be 16 MB)
    let mut out = Vec::with_capacity(s.len());
    let put = |out: &mut Vec<u8>, cs: &mut dyn Iterator<Item = char>| {
        for c in cs {
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    };
    match op {
        // (a Greek capital sigma lowercases by the letters around it, which only the whole text knows)
        CaseOp::Lower if s.contains('Σ') => return s.to_lowercase().into_bytes(),
        CaseOp::Upper | CaseOp::Lower => {
            let upper = op == CaseOp::Upper;
            let mut rest = s;
            while !rest.is_empty() {
                let k = rest.bytes().position(|b| !b.is_ascii()).unwrap_or(rest.len());
                let from = out.len();
                out.extend_from_slice(&rest.as_bytes()[..k]);
                if upper { out[from..].make_ascii_uppercase() } else { out[from..].make_ascii_lowercase() }
                let wide = &rest[k..];
                let m = wide.bytes().position(|b| b.is_ascii()).unwrap_or(wide.len());
                for c in wide[..m].chars() {
                    if upper { put(&mut out, &mut c.to_uppercase()) } else { put(&mut out, &mut c.to_lowercase()) }
                }
                rest = &wide[m..];
            }
        }
        CaseOp::Title => {
            let mut start = true;
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if c.is_ascii() {
                        out.push(if start { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() } as u8);
                    } else if start {
                        put(&mut out, &mut c.to_uppercase());
                    } else {
                        put(&mut out, &mut c.to_lowercase());
                    }
                    start = false;
                } else {
                    put(&mut out, &mut std::iter::once(c));
                    // "don't" stays one word; spaces, dashes and brackets start a new one
                    if c.is_whitespace() || "-_/([{\"".contains(c) {
                        start = true;
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(op: LineOp, text: &str) -> (String, u64) {
        let (out, n) = apply(op, text.as_bytes());
        (String::from_utf8(out).unwrap(), n)
    }

    #[test]
    fn sorting_is_natural_and_ignores_case() {
        assert_eq!(run(LineOp::SortAsc, "file10\nFile2\nfile1\nbanana\nApple\n"), ("Apple\nbanana\nfile1\nFile2\nfile10\n".into(), 5));
        assert_eq!(run(LineOp::SortDesc, "b\na\nc").0, "c\nb\na");
        assert_eq!(run(LineOp::SortAsc, "a\nb\n").1, 0);
        // CRLF text stays CRLF, also for a last line without a line break
        assert_eq!(run(LineOp::SortAsc, "b\r\nc\r\na").0, "a\r\nb\r\nc");
        assert_eq!(natural_cmp(b"x007", b"x7"), Ordering::Equal);
        assert_eq!(natural_cmp(b"v1.10", b"v1.9"), Ordering::Greater);
    }

    #[test]
    fn numbers_and_accents_sort_as_people_expect() {
        let sorted = |t: &str| run(LineOp::SortAsc, t).0;
        assert_eq!(sorted("10\n9\n-5\n-10\n1.5\n1.25\n-0.5\n"), "-10\n-5\n-0.5\n1.25\n1.5\n9\n10\n");
        assert_eq!(sorted("$9.99\n$10.25\n$10.5\n"), "$9.99\n$10.25\n$10.5\n");
        // versions and addresses go by their parts; a dash inside a word or date isn't a minus
        assert_eq!(sorted("1.10.0\n1.2.3\n1.9.9\n"), "1.2.3\n1.9.9\n1.10.0\n");
        assert_eq!(sorted("10.0.0.2\n9.1.1.1\n10.0.0.10\n"), "9.1.1.1\n10.0.0.2\n10.0.0.10\n");
        assert_eq!(sorted("2026-10-07\n2026-9-30\n2025-12-31\n"), "2025-12-31\n2026-9-30\n2026-10-07\n");
        assert_eq!(sorted("x-5\nx-10\n"), "x-5\nx-10\n");
        // accented letters next to their plain one; between equal words, plain first
        assert_eq!(sorted("Zoe\némile\nEve\nÄrger\narger\nzebra\nŁódź\nlodz\n"), "arger\nÄrger\némile\nEve\nlodz\nŁódź\nzebra\nZoe\n");
        assert_eq!(sorted("Straße\nstrasse\nStrand\nStrasbourg\n"), "Strand\nStrasbourg\nStraße\nstrasse\n");
        assert_eq!(sorted("Émile\nemile\n"), "emile\nÉmile\n");
        // other scripts fold case too, and bytes that aren't UTF-8 go last
        assert_eq!(natural_cmp("Ωmega".as_bytes(), "ωmega".as_bytes()), Ordering::Equal);
        assert_eq!(natural_cmp(b"a\xFF", "aé".as_bytes()), Ordering::Greater);
    }

    #[test]
    fn natural_order_is_a_total_order() {
        // sort_by needs a consistent order (or it may panic): check it on random lines
        let parts = ["-", "1", "10", "0.5", ".", "a", "A", "é", "É", "x", " ", "-5", "2.3.4", "_", "\u{FF}", "ß", "Z"];
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let lines: Vec<Vec<u8>> = (0..200)
            .map(|_| (0..1 + next() % 4).flat_map(|_| parts[(next() % parts.len() as u64) as usize].bytes()).collect())
            .collect();
        for a in &lines {
            for b in &lines {
                assert_eq!(natural_cmp(a, b), natural_cmp(b, a).reverse(), "{a:?} {b:?}");
                for c in lines.iter().take(40) {
                    if natural_cmp(a, b).is_le() && natural_cmp(b, c).is_le() {
                        assert!(natural_cmp(a, c).is_le(), "{a:?} <= {b:?} <= {c:?}");
                    }
                }
            }
        }
        let mut v = lines.clone();
        v.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
        // (`apply` gives that order too)
        let text: Vec<u8> = lines.iter().flat_map(|l| [l.as_slice(), b"\n"].concat()).collect();
        let want: Vec<u8> = v.iter().flat_map(|l| [l.as_slice(), b"\n"].concat()).collect();
        assert!(apply(LineOp::SortAsc, &text).0 == want);
        // what two lines start with is skipped: the same order as comparing them from the start
        let heads = ["", "x", "file-1", "v1.2", "2026-10-0", "Éa", "a b ", "-"];
        let long: Vec<Vec<u8>> = lines.iter().enumerate().map(|(k, l)| [heads[k % heads.len()].as_bytes(), l].concat()).collect();
        for a in long.iter().chain(&lines) {
            for b in long.iter().chain(&lines) {
                let whole = natural(a, b, 0, true).then_with(|| natural(a, b, 0, false));
                assert_eq!(natural_cmp(a, b), whole, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn cleaning_lines() {
        assert_eq!(run(LineOp::Dedupe, "a\nb\na\nc\nb\n"), ("a\nb\nc\n".into(), 2));
        assert_eq!(run(LineOp::RemoveBlank, "a\n\n  \nb\n\t\n"), ("a\nb\n".into(), 3));
        assert_eq!(run(LineOp::RemoveBlank, "\n\n").0, "");
        assert_eq!(run(LineOp::TrimTrailing, "a  \nb\t\nc\r\n"), ("a\nb\nc\n".into(), 2));
        assert_eq!(run(LineOp::TrimTrailing, "keep  \r\nmore\r\n"), ("keep\r\nmore\r\n".into(), 1));
    }

    #[test]
    fn letter_case_a_run_at_a_time_is_the_same() {
        // (ASCII goes a run at a time: the same as converting it all in one go)
        let parts = ["abc DEF", " ", "Grüße", "ß", "İstanbul", "ΟΔΟΣ", "Σ", "aΣ b", "ŉ", "日本", "x-y_z", "\"q\"", "ǅ", "1a"];
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..300 {
            let mut s = String::new();
            for _ in 0..1 + r % 9 {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                s.push_str(parts[(r % parts.len() as u64) as usize]);
            }
            assert_eq!(change_case(s.as_bytes(), CaseOp::Upper), s.to_uppercase().as_bytes(), "{s}");
            assert_eq!(change_case(s.as_bytes(), CaseOp::Lower), s.to_lowercase().as_bytes(), "{s}");
            let mut title = String::new();
            let mut start = true;
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if start { title.extend(c.to_uppercase()) } else { title.extend(c.to_lowercase()) }
                    start = false;
                } else {
                    title.push(c);
                    start |= c.is_whitespace() || "-_/([{\"".contains(c);
                }
            }
            assert_eq!(change_case(s.as_bytes(), CaseOp::Title), title.as_bytes(), "{s}");
        }
    }

    #[test]
    fn letter_case() {
        assert_eq!(change_case("Grüße, Wörld".as_bytes(), CaseOp::Upper), "GRÜSSE, WÖRLD".as_bytes());
        assert_eq!(change_case(b"MiXeD", CaseOp::Lower), b"mixed");
        assert_eq!(change_case(b"don't stop-me now", CaseOp::Title), b"Don't Stop-Me Now");
        assert_eq!(change_case(&[b'a', 0xFF, b'b'], CaseOp::Upper), vec![b'A', 0xFF, b'B']);
        assert_eq!(change_case(&[b'h', b'I', b' ', b'y', 0xFF, b'O'], CaseOp::Title), vec![b'H', b'i', b' ', b'Y', 0xFF, b'o']);
    }
}
