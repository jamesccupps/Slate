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

/// Compares the way people sort: letters ignoring case, numbers by value ("file2" before "file10").
pub fn natural_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let x = trim_zeros(&a[si..i]);
            let y = trim_zeros(&b[sj..j]);
            let c = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
            if c != Ordering::Equal {
                return c;
            }
            continue;
        }
        let c = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
        if c != Ordering::Equal {
            return c;
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&b| b == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
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
            lines.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
            if op == LineOp::SortDesc {
                lines.reverse();
            }
            count = before;
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
            _ => text.to_ascii_uppercase(),
        };
    };
    match op {
        CaseOp::Upper => s.to_uppercase().into_bytes(),
        CaseOp::Lower => s.to_lowercase().into_bytes(),
        CaseOp::Title => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    start = false;
                } else {
                    out.push(c);
                    // "don't" stays one word; spaces, dashes and brackets start a new one
                    if c.is_whitespace() || "-_/([{\"".contains(c) {
                        start = true;
                    }
                }
            }
            out.into_bytes()
        }
    }
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
        // CRLF text stays CRLF, also for a last line without a line break
        assert_eq!(run(LineOp::SortAsc, "b\r\nc\r\na").0, "a\r\nb\r\nc");
        assert_eq!(natural_cmp(b"x007", b"x7"), Ordering::Equal);
        assert_eq!(natural_cmp(b"v1.10", b"v1.9"), Ordering::Greater);
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
    fn letter_case() {
        assert_eq!(change_case("Grüße, Wörld".as_bytes(), CaseOp::Upper), "GRÜSSE, WÖRLD".as_bytes());
        assert_eq!(change_case(b"MiXeD", CaseOp::Lower), b"mixed");
        assert_eq!(change_case(b"don't stop-me now", CaseOp::Title), b"Don't Stop-Me Now");
        assert_eq!(change_case(&[b'a', 0xFF, b'b'], CaseOp::Upper), vec![b'A', 0xFF, b'B']);
    }
}
