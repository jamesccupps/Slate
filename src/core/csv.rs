//! CSV and TSV: Check (RFC 4180's quoting: a field in quotes may hold the separator, line breaks and `""` for a
//! quote), Sort by a column (rows kept whole, a header row on top) and the field a place is in (the status bar names
//! the caret's column). The separator is the file's language's (`;` for "CSV (semicolons)", a tab for TSV), or for
//! plain CSV the one of `,` `;` tab `|` that splits its first lines alike (`sniff`). Check streams through the text,
//! jumping from one special byte to the next (`memchr`), so a big file is checked about as fast as it's searched.

use std::cmp::Ordering;
use std::io;

use super::buffer::Snapshot;
use super::job::Ctx;
use super::lines::natural_cmp;

/// How a file's fields are separated and quoted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    pub sep: u8,
    /// A quote inside a field that doesn't start with one is a mistake (CSV, RFC 4180). TSV has no quoting rules of
    /// its own: there a quote is only special at a field's start, as spreadsheets write them, else it's text.
    pub strict: bool,
}

impl Dialect {
    pub fn csv(sep: u8) -> Self {
        Dialect { sep, strict: true }
    }

    pub fn tsv() -> Self {
        Dialect { sep: b'\t', strict: false }
    }

    /// "CSV" or "TSV", for messages.
    pub fn name(self) -> &'static str {
        if self.sep == b'\t' && !self.strict { "TSV" } else { "CSV" }
    }
}

/// The separator of a CSV file from its first lines (`head`): of `,` `;` tab `|`, the one that splits the most of
/// its first 20 rows into as many fields as the first row (more than one), then the one giving more fields; `,` when
/// none does.
pub fn sniff(head: &[u8]) -> u8 {
    let mut best = (b',', 0usize, 0usize);
    for sep in [b',', b';', b'\t', b'|'] {
        let counts = row_widths(head, Dialect::csv(sep), 20);
        let Some(&first) = counts.first() else { continue };
        if first < 2 {
            continue;
        }
        let alike = counts.iter().filter(|&&n| n == first).count();
        if (alike, first) > (best.1, best.2) {
            best = (sep, alike, first);
        }
    }
    best.0
}

/// How many fields each of the first `max` rows of `text` has (rows that end in `text`; blank lines skipped).
fn row_widths(text: &[u8], d: Dialect, max: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut rows = Rows::new(text, d);
    while out.len() < max {
        let Some(r) = rows.next() else { break };
        if r.end_nl && r.end > r.start {
            out.push(r.fields);
        }
    }
    out
}

// ---- Check ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvError {
    pub offset: u64,
    pub msg: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Rows, the first (a header or not) included; blank lines aren't rows.
    pub rows: u64,
    /// Fields in each row.
    pub fields: usize,
}

pub const QUOTE_OPEN: &str = "A quote opened here is never closed";
pub const STRAY_QUOTE: &str =
    "A quote inside a field that isn't in quotes (in CSV such a field is put in quotes, and each quote in it doubled: \"\")";
pub const AFTER_QUOTE: &str =
    "Something after a field's closing quote (a quote inside a field in quotes is written twice: \"\")";

/// "This row has 7 fields; the first has 6".
pub fn count_msg(n: usize, want: usize) -> String {
    let fields = |n: usize| if n == 1 { "1 field".to_string() } else { format!("{n} fields") };
    format!("This row has {}; the first row has {want}", fields(n))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    /// At a field's start.
    Start,
    /// In a field without quotes.
    Plain,
    /// In a field in quotes.
    Quoted,
    /// Just after a quote in a field in quotes: a second one, or the field's end.
    Quote,
    /// A `\r` after a field's closing quote: a `\n` must follow.
    QuoteCr,
}

struct Checker {
    d: Dialect,
    st: St,
    /// The offset of the next byte.
    pos: u64,
    /// Separators seen in this row.
    seps: usize,
    /// Whether this row has anything but `\r` (a blank line isn't a row).
    filled: bool,
    want: Option<usize>,
    rows: u64,
    /// Where the open quote was.
    quote_at: u64,
    /// The separator that started the first field too many in this row.
    extra_at: Option<u64>,
}

impl Checker {
    fn new(d: Dialect) -> Self {
        Checker { d, st: St::Start, pos: 0, seps: 0, filled: false, want: None, rows: 0, quote_at: 0, extra_at: None }
    }

    fn err(at: u64, msg: impl Into<String>) -> CsvError {
        CsvError { offset: at, msg: msg.into() }
    }

    /// A separator at `at`.
    fn sep(&mut self, at: u64) {
        self.seps += 1;
        self.filled = true;
        if self.extra_at.is_none() && self.want == Some(self.seps) {
            self.extra_at = Some(at);
        }
        self.st = St::Start;
    }

    /// The row ends at `at` (its line break, or the text's end).
    fn row_end(&mut self, at: u64) -> Result<(), CsvError> {
        let n = self.seps + 1;
        let filled = std::mem::replace(&mut self.filled, false);
        let extra = self.extra_at.take();
        self.seps = 0;
        self.st = St::Start;
        if !filled {
            return Ok(());
        }
        self.rows += 1;
        match self.want {
            None => self.want = Some(n),
            Some(w) if n > w => return Err(Self::err(extra.unwrap_or(at), count_msg(n, w))),
            Some(w) if n < w => return Err(Self::err(at, count_msg(n, w))),
            _ => {}
        }
        Ok(())
    }

    fn feed(&mut self, c: &[u8]) -> Result<(), CsvError> {
        let base = self.pos;
        let (sep, strict) = (self.d.sep, self.d.strict);
        let mut i = 0usize;
        while i < c.len() {
            let at = base + i as u64;
            if self.st == St::Start && self.seps == 0 && !self.filled {
                // At a row's start: a row with no quote in it has its separators counted at once (a call per field
                // took twice as long). One that doesn't have the fields wanted goes byte by byte, which says where.
                if let Some(k) = memchr::memchr(b'\n', &c[i..]) {
                    let row = &c[i..i + k];
                    if memchr::memchr(b'"', row).is_none() {
                        let n = bytecount::count(row, sep) + 1;
                        if row.iter().all(|&b| b == b'\r') {
                            i += k + 1;
                            continue;
                        }
                        if self.want.is_none_or(|w| w == n) {
                            self.want = Some(n);
                            self.rows += 1;
                            i += k + 1;
                            continue;
                        }
                    }
                }
            }
            match self.st {
                St::Start => {
                    let b = c[i];
                    i += 1;
                    if b == b'"' {
                        self.quote_at = at;
                        self.filled = true;
                        self.st = St::Quoted;
                    } else if b == sep {
                        self.sep(at);
                    } else if b == b'\n' {
                        // (a `\r` before it was taken as a field's text: it's part of the line break)
                        self.row_end(at)?;
                    } else {
                        if b != b'\r' {
                            self.filled = true;
                        }
                        self.st = St::Plain;
                    }
                }
                St::Plain => {
                    let rest = &c[i..];
                    let hit = if strict { memchr::memchr3(sep, b'\n', b'"', rest) } else { memchr::memchr2(sep, b'\n', rest) };
                    let Some(k) = hit else {
                        // (only `\r`s are no field's text yet: a blank line ending in CRLF)
                        if !self.filled && rest.iter().any(|&b| b != b'\r') {
                            self.filled = true;
                        }
                        break;
                    };
                    if !self.filled && rest[..k].iter().any(|&b| b != b'\r') {
                        self.filled = true;
                    }
                    let at = at + k as u64;
                    i += k + 1;
                    match rest[k] {
                        b'\n' => self.row_end(at)?,
                        b'"' => return Err(Self::err(at, STRAY_QUOTE)),
                        _ => self.sep(at),
                    }
                }
                St::Quoted => match memchr::memchr(b'"', &c[i..]) {
                    Some(k) => {
                        i += k + 1;
                        self.st = St::Quote;
                    }
                    None => i = c.len(),
                },
                St::Quote => {
                    let b = c[i];
                    i += 1;
                    match b {
                        b'"' => self.st = St::Quoted,
                        b'\n' => self.row_end(at)?,
                        b'\r' => self.st = St::QuoteCr,
                        _ if b == sep => self.sep(at),
                        _ => return Err(Self::err(at, AFTER_QUOTE)),
                    }
                }
                St::QuoteCr => {
                    if c[i] != b'\n' {
                        return Err(Self::err(at - 1, AFTER_QUOTE));
                    }
                    i += 1;
                    self.row_end(at)?;
                }
            }
        }
        self.pos = base + c.len() as u64;
        Ok(())
    }

    fn finish(mut self) -> Result<Stats, CsvError> {
        match self.st {
            St::Quoted => return Err(Self::err(self.quote_at, QUOTE_OPEN)),
            St::QuoteCr => return Err(Self::err(self.pos - 1, AFTER_QUOTE)),
            _ => {}
        }
        let end = self.pos;
        self.row_end(end)?;
        Ok(Stats { rows: self.rows, fields: self.want.unwrap_or(0) })
    }
}

/// Checks the whole text as `d`: every row as many fields as the first, every quote where it may be.
pub fn check(snap: &Snapshot, d: Dialect, ctx: &Ctx) -> Result<Stats, CsvError> {
    let mut c = Checker::new(d);
    let mut pos = 0u64;
    let mut res = Ok(());
    while pos < snap.len() && res.is_ok() {
        if ctx.cancelled() {
            return Err(Checker::err(pos, "Cancelled"));
        }
        let end = (pos + (8 << 20)).min(snap.len());
        snap.chunks(pos, end, &mut |piece| {
            res = c.feed(piece);
            res.is_ok()
        });
        pos = end;
        ctx.set(pos);
    }
    res?;
    c.finish()
}

/// Check over bytes in memory (tests, and small texts).
pub fn check_bytes(text: &[u8], d: Dialect) -> Result<Stats, CsvError> {
    let mut c = Checker::new(d);
    c.feed(text)?;
    c.finish()
}

// ---- rows and fields ----

/// A row of a text in memory: where it starts and ends (before its line break), its fields, and whether a line
/// break ended it.
#[derive(Clone, Copy, Debug)]
struct RowSpan {
    start: usize,
    end: usize,
    fields: usize,
    end_nl: bool,
}

/// The rows of a text in memory, quotes respected (a row goes on over line breaks inside quotes; a quote left open
/// takes the rest of the text). Lenient: whatever Check would say, every byte is in some row.
struct Rows<'a> {
    text: &'a [u8],
    d: Dialect,
    at: usize,
}

impl<'a> Rows<'a> {
    fn new(text: &'a [u8], d: Dialect) -> Self {
        Rows { text, d, at: 0 }
    }

    /// The next row; `key`: also where field `key` is in it (inside its quotes, if it has them).
    fn next_with(&mut self, key: Option<usize>, key_at: &mut (usize, usize)) -> Option<RowSpan> {
        let t = self.text;
        if self.at >= t.len() {
            return None;
        }
        let start = self.at;
        let mut i = start;
        let mut field = 0usize;
        let mut field_start = i;
        *key_at = (start, start);
        let mut quoted_key = None;
        loop {
            // a field: in quotes, or up to the next separator or line break
            if i < t.len() && t[i] == b'"' {
                let open = i;
                i += 1;
                loop {
                    match memchr::memchr(b'"', &t[i..]) {
                        Some(k) if t.get(i + k + 1) == Some(&b'"') => i += k + 2,
                        Some(k) => {
                            if Some(field) == key {
                                quoted_key = Some((open + 1, i + k));
                            }
                            i += k + 1;
                            break;
                        }
                        None => {
                            i = t.len();
                            break;
                        }
                    }
                }
            }
            let next = memchr::memchr2(self.d.sep, b'\n', &t[i.min(t.len())..]).map(|k| i + k);
            let end = next.unwrap_or(t.len());
            if Some(field) == key {
                let mut e = end;
                if t.get(e) == Some(&b'\n') && e > field_start && t[e - 1] == b'\r' {
                    e -= 1;
                }
                *key_at = quoted_key.unwrap_or((field_start, e));
            }
            match next {
                Some(p) if t[p] == self.d.sep => {
                    field += 1;
                    i = p + 1;
                    field_start = i;
                }
                Some(p) => {
                    self.at = p + 1;
                    let end = if p > start && t[p - 1] == b'\r' { p - 1 } else { p };
                    return Some(RowSpan { start, end, fields: field + 1, end_nl: true });
                }
                None => {
                    self.at = t.len();
                    return Some(RowSpan { start, end: t.len(), fields: field + 1, end_nl: false });
                }
            }
        }
    }

    fn next(&mut self) -> Option<RowSpan> {
        let mut k = (0, 0);
        self.next_with(None, &mut k)
    }
}

/// The fields of a row (a header's names): unquoted, `""` as `"`, at most `max` of them.
pub fn fields(row: &[u8], d: Dialect, max: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while out.len() < max {
        let mut f = Vec::new();
        if row.get(i) == Some(&b'"') {
            i += 1;
            while i < row.len() {
                if row[i] == b'"' {
                    if row.get(i + 1) == Some(&b'"') {
                        f.push(b'"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                f.push(row[i]);
                i += 1;
            }
        }
        let end = memchr::memchr2(d.sep, b'\n', &row[i.min(row.len())..]).map_or(row.len(), |k| i + k);
        f.extend_from_slice(&row[i.min(end)..end]);
        if f.last() == Some(&b'\r') {
            f.pop();
        }
        out.push(f);
        if end >= row.len() || row[end] == b'\n' {
            break;
        }
        i = end + 1;
    }
    out
}

/// The first row of a text (its first 64 KiB, say), quotes respected, without its line break.
pub fn first_row(head: &[u8], d: Dialect) -> &[u8] {
    match Rows::new(head, d).next() {
        Some(r) => &head[r.start..r.end],
        None => &[],
    }
}

/// Whether a first row reads as a header: none of its fields is a number or a date (data rows have some).
pub fn has_header(row: &[u8], d: Dialect) -> bool {
    !row.trim_ascii().is_empty() && fields(row, d, usize::MAX).iter().all(|f| !numeric(f))
}

/// Which field (from 0) the place `upto.len()` is in, `upto` being its line up to it (quotes respected from the
/// line's start: a line that goes on a field in quotes from the line before is counted as a row of its own).
pub fn field_at(upto: &[u8], d: Dialect) -> usize {
    let mut n = 0usize;
    let mut quoted = false;
    let mut at_start = true;
    for &b in upto {
        if quoted {
            if b == b'"' {
                quoted = false;
            }
        } else if b == d.sep {
            n += 1;
            at_start = true;
            continue;
        } else if b == b'"' && at_start {
            quoted = true;
        }
        at_start = false;
    }
    n
}

// ---- Sort by a column ----

/// Whether `s` reads as a number or a date (`-1,234.5`, `12%`, `$3`, `12/31/2026`, `10:30`): such a field in the
/// first row means it's data, not a header.
fn numeric(s: &[u8]) -> bool {
    let s = s.trim_ascii();
    let s = s.strip_prefix(b"-").or_else(|| s.strip_prefix(b"+")).unwrap_or(s);
    let s = s.strip_prefix(b"$").unwrap_or(s);
    let s = s.strip_suffix(b"%").unwrap_or(s);
    !s.is_empty() && s[0].is_ascii_digit() && s.iter().all(|&b| b.is_ascii_digit() || b == b'.' || b == b',' || b == b'-' || b == b'/' || b == b':' || b == b' ')
}

/// A date written `month/day/year` or `day/month/year` (`/`, `.` or `-` between, a four-digit year last), as
/// (year, month, day) and what follows (a time); `day_first` from the system's date order.
fn date_key(s: &[u8], day_first: bool) -> Option<((u32, u32, u32), &[u8])> {
    let num = |s: &[u8], i: &mut usize, max: usize| -> Option<u32> {
        let from = *i;
        while *i < s.len() && s[*i].is_ascii_digit() && *i - from < max {
            *i += 1;
        }
        if *i == from { None } else { std::str::from_utf8(&s[from..*i]).ok()?.parse().ok() }
    };
    let mut i = 0;
    let a = num(s, &mut i, 2)?;
    let sep = *s.get(i)?;
    if !matches!(sep, b'/' | b'.' | b'-') {
        return None;
    }
    i += 1;
    let b = num(s, &mut i, 2)?;
    if s.get(i) != Some(&sep) {
        return None;
    }
    i += 1;
    let from = i;
    let y = num(s, &mut i, 4)?;
    if i - from != 4 || s.get(i).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let (m, d) = if day_first { (b, a) } else { (a, b) };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(((y, m, d), &s[i..]))
}

/// The order of two fields: dates by date, numbers by value, text the way people sort (`natural_cmp`).
fn field_order(a: &[u8], b: &[u8], day_first: bool) -> Ordering {
    if let (Some((x, xr)), Some((y, yr))) = (date_key(a, day_first), date_key(b, day_first)) {
        return x.cmp(&y).then_with(|| natural_cmp(xr, yr));
    }
    natural_cmp(a.trim_ascii(), b.trim_ascii()).then_with(|| a.cmp(b))
}

#[derive(Clone, Copy)]
struct SortRow {
    start: u32,
    end: u32,
    ks: u32,
    ke: u32,
    /// Its place before sorting: equal fields keep their order.
    n: u32,
    /// The field as a date (`year * 10000 + month * 100 + day`), 0 if it isn't one: worked out once per row, not on
    /// every comparison.
    date: u32,
}

/// What `sort_by_column` did: rows sorted (0: they were in order already), and whether the first row stayed on top
/// as a header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sorted {
    pub rows: u64,
    pub header: bool,
}

/// Sorts the rows of `text` by field `col` (from 0): dates, numbers and text as `field_order` has them, empty fields
/// last either way, rows with the same field in their order. `header`: the first row stays on top (`has_header`).
/// Rows are kept whole, line breaks in quotes too; they come out with the line break most of the text uses, handed to
/// `out` in pieces as `lines::apply_to` does. Texts up to 4 GB.
pub fn sort_by_column(
    text: &[u8],
    d: Dialect,
    col: usize,
    desc: bool,
    day_first: bool,
    header: bool,
    out: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Sorted> {
    let crlf = memchr::memmem::find_iter(text, b"\r\n").count() * 2 > memchr::memchr_iter(b'\n', text).count();
    let ends_nl = text.last() == Some(&b'\n');
    let mut rows: Vec<SortRow> = Vec::new();
    let mut it = Rows::new(text, d);
    let mut key = (0, 0);
    while let Some(r) = it.next_with(Some(col), &mut key) {
        let (ks, ke) = if col < r.fields { key } else { (r.end, r.end) };
        let n = rows.len() as u32;
        let date = date_key(&text[ks..ke], day_first).map_or(0, |((y, m, d), _)| y * 10000 + m * 100 + d);
        rows.push(SortRow { start: r.start as u32, end: r.end as u32, ks: ks as u32, ke: ke as u32, n, date });
    }
    let header = header && !rows.is_empty();
    let body = if header { 1 } else { 0 };
    let key_of = |r: &SortRow| &text[r.ks as usize..r.ke as usize];
    let cmp = |a: &SortRow, b: &SortRow| {
        let (x, y) = (key_of(a), key_of(b));
        let (ex, ey) = (x.trim_ascii().is_empty(), y.trim_ascii().is_empty());
        match (ex, ey) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (true, true) => a.n.cmp(&b.n),
            _ => {
                let o = if a.date != 0 && b.date != 0 {
                    // (the same day: by what follows, a time)
                    a.date.cmp(&b.date).then_with(|| field_order(x, y, day_first))
                } else {
                    natural_cmp(x.trim_ascii(), y.trim_ascii()).then_with(|| x.cmp(y))
                };
                (if desc { o.reverse() } else { o }).then(a.n.cmp(&b.n))
            }
        }
    };
    // (the rows to sort taken out, the header left: no copy of them all beside the sort's own)
    let mut part = rows.split_off(body);
    let in_order = part.windows(2).all(|w| cmp(&w[0], &w[1]) != Ordering::Greater);
    let mut count = 0;
    if !in_order {
        super::lines::par_sort_by(&mut part, cmp);
        count = part.len() as u64;
    }
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let piece_max = if cfg!(test) { 16 } else { 4 << 20 };
    let mut piece = Vec::with_capacity(piece_max.min(text.len() + 2));
    for (k, r) in rows.iter().chain(&part).enumerate() {
        if k > 0 {
            piece.extend_from_slice(eol);
        }
        piece.extend_from_slice(&text[r.start as usize..r.end as usize]);
        if piece.len() >= piece_max {
            out(&piece)?;
            piece.clear();
        }
    }
    if ends_nl && !(rows.is_empty() && part.is_empty()) {
        piece.extend_from_slice(eol);
    }
    out(&piece)?;
    Ok(Sorted { rows: count, header })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chk(text: &str) -> Result<Stats, CsvError> {
        check_bytes(text.as_bytes(), Dialect::csv(b','))
    }

    /// Check fed a byte at a time gives what it gives in one piece.
    fn chk_split(text: &str, d: Dialect) -> Result<Stats, CsvError> {
        let mut c = Checker::new(d);
        for b in text.as_bytes() {
            c.feed(std::slice::from_ref(b))?;
        }
        c.finish()
    }

    #[test]
    fn valid_files_are_counted() {
        let ok = "name,qty,note\r\nbolt,3,\"a, b\"\r\nnut,\"\"\"4\"\"\",\"two\r\nlines\"\r\n\r\nwasher,5,\n";
        assert_eq!(chk(ok), Ok(Stats { rows: 4, fields: 3 }));
        assert_eq!(chk_split(ok, Dialect::csv(b',')), Ok(Stats { rows: 4, fields: 3 }));
        // no line break at the end, one column, empty
        assert_eq!(chk("a,b\n1,2"), Ok(Stats { rows: 2, fields: 2 }));
        assert_eq!(chk("x\ny\n"), Ok(Stats { rows: 2, fields: 1 }));
        assert_eq!(chk(""), Ok(Stats { rows: 0, fields: 0 }));
        assert_eq!(chk("\r\n\n"), Ok(Stats { rows: 0, fields: 0 }));
    }

    #[test]
    fn mistakes_are_found_where_they_are() {
        let at = |text: &str| chk(text).unwrap_err();
        let e = at("a,b,c\n1,2,3\n4,5\n");
        assert_eq!((e.offset, e.msg.as_str()), (15, "This row has 2 fields; the first row has 3"));
        // too many: at the separator starting the first field too many
        let e = at("a,b\n1,2,3,4\n");
        assert_eq!((e.offset, e.msg.as_str()), (7, "This row has 4 fields; the first row has 2"));
        assert_eq!(at("a,b\n1,\"open\n").offset, 6);
        assert_eq!(at("a,b\n1,\"open\n").msg, QUOTE_OPEN);
        assert_eq!(at("a,b\n1,x\"y\n"), CsvError { offset: 7, msg: STRAY_QUOTE.into() });
        assert_eq!(at("a,b\n1,\"x\"y\n"), CsvError { offset: 9, msg: AFTER_QUOTE.into() });
        assert_eq!(at("a,b\n\"x\"\r1,2\n"), CsvError { offset: 7, msg: AFTER_QUOTE.into() });
        // the same a byte at a time
        for text in ["a,b,c\n1,2,3\n4,5\n", "a,b\n1,\"x\"y\n", "a,b\n\"x\"\r1,2\n", "a,b\n1,\"open\n", "a,b\n1,x\"y\n"] {
            assert_eq!(chk_split(text, Dialect::csv(b',')), chk(text), "{text:?}");
        }
    }

    #[test]
    fn tsv_quotes_are_text_inside_a_field() {
        let tsv = Dialect::tsv();
        assert_eq!(check_bytes(b"size\tnote\n5\"\tpipe 5\" long\n", tsv), Ok(Stats { rows: 2, fields: 2 }));
        assert!(check_bytes(b"a\tb\n\"x\"y\t1\n", tsv).is_err());
        assert_eq!(check_bytes(b"a\tb\n\"x\ty\"\t1\n", tsv), Ok(Stats { rows: 2, fields: 2 }));
    }

    #[test]
    fn the_separator_is_found_from_the_first_lines() {
        assert_eq!(sniff(b"a,b,c\n1,2,3\n"), b',');
        assert_eq!(sniff(b"a;b;c\n1,5;2;3\n"), b';');
        assert_eq!(sniff(b"a\tb\n1\t2\n"), b'\t');
        assert_eq!(sniff(b"a|b|c\n1|2|3\n4|5|6\n"), b'|');
        assert_eq!(sniff(b"\"x, y\";b\n1;2\n"), b';');
        assert_eq!(sniff(b"just text\n"), b',');
    }

    #[test]
    fn fields_and_the_field_at_a_place() {
        let d = Dialect::csv(b',');
        assert_eq!(fields(b"id,\"the \"\"name\"\"\",x\r\n", d, 9), [&b"id"[..], b"the \"name\"", b"x"]);
        assert_eq!(fields(b"a,b,c", d, 2).len(), 2);
        assert_eq!(field_at(b"", d), 0);
        assert_eq!(field_at(b"1,\"a,b", d), 1);
        assert_eq!(field_at(b"1,\"a,b\",x", d), 2);
        assert_eq!(field_at(b"a\tb\tc", Dialect::tsv()), 2);
        assert_eq!(first_row(b"\"a\nb\",c\r\n1,2\n", d), b"\"a\nb\",c");
        assert!(has_header(b"id,name,\"when, where\"", d));
        assert!(!has_header(b"bolt,3,x", d) && !has_header(b"a,12/31/2026", d) && !has_header(b"", d));
    }

    fn sort(text: &str, col: usize, desc: bool) -> (String, Sorted) {
        let mut out = Vec::new();
        let d = Dialect::csv(b',');
        let header = has_header(first_row(text.as_bytes(), d), d);
        let s = sort_by_column(text.as_bytes(), d, col, desc, false, header, &mut |p| {
            out.extend_from_slice(p);
            Ok(())
        })
        .unwrap();
        (String::from_utf8(out).unwrap(), s)
    }

    #[test]
    fn rows_are_sorted_by_a_column() {
        let text = "item,qty,when\nb,10,1/5/2027\na,9,12/31/2026\n\"c\nd\",,3/1/2026\ne,-1,\n";
        let (s, r) = sort(text, 1, false);
        assert_eq!(s, "item,qty,when\ne,-1,\na,9,12/31/2026\nb,10,1/5/2027\n\"c\nd\",,3/1/2026\n");
        assert_eq!(r, Sorted { rows: 4, header: true });
        // dates as dates, the empty field last also from Z to A
        let (s, _) = sort(text, 2, true);
        assert_eq!(s, "item,qty,when\nb,10,1/5/2027\na,9,12/31/2026\n\"c\nd\",,3/1/2026\ne,-1,\n");
        // in order already: nothing sorted; no header when the first row has numbers
        assert_eq!(sort("a,1\nb,2\n", 0, false).1, Sorted { rows: 0, header: false });
        assert_eq!(sort("b,1\r\na,2", 0, false).0, "a,2\r\nb,1");
        // a quoted field sorts by what's inside the quotes; equal ones keep their order
        assert_eq!(sort("k,v\n\"b\",1\na,2\nb,3\n", 0, false).0, "k,v\na,2\n\"b\",1\nb,3\n");
    }

    /// `SLATE_CSV_FILE=<file> cargo test --release --lib csv_speed -- --ignored --nocapture`: Check and a sort by
    /// the second column, against counting the line breaks (reading alone).
    #[test]
    #[ignore]
    fn csv_speed() {
        use super::super::buffer::Buffer;
        use super::super::source::Source;
        let Some(p) = std::env::var_os("SLATE_CSV_FILE") else { return };
        let text = std::fs::read(p).unwrap();
        let d = Dialect::csv(sniff(&text[..text.len().min(64 << 10)]));
        let ctx = Ctx { cancel: Default::default(), progress: Default::default() };
        let snap = Buffer::from_source(std::sync::Arc::new(Source::from_vec(text.clone())), 0).snapshot();
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let n = bytecount::count(&text, b'\n');
            let count = t.elapsed();
            let t = std::time::Instant::now();
            let r = check(&snap, d, &ctx);
            let checked = t.elapsed();
            let t = std::time::Instant::now();
            let s = sort_by_column(&text, d, 1, false, false, true, &mut |_| Ok(())).unwrap();
            println!("{n} lines counted {count:?}; check {checked:?} ({r:?}); sort by column 2 {:?} ({s:?})", t.elapsed());
        }
    }

    #[test]
    fn dates_in_either_order() {
        assert_eq!(date_key(b"12/31/2026", false), Some(((2026, 12, 31), &b""[..])));
        assert_eq!(date_key(b"31.12.2026 10:00", true), Some(((2026, 12, 31), &b" 10:00"[..])));
        assert_eq!(date_key(b"31/12/2026", false), None);
        assert_eq!(date_key(b"1/2/26", false), None);
        assert_eq!(date_key(b"2026-10-13", false), None);
    }
}
