//! Configuration and other line-based formats: TOML, nginx and Apache configuration, Java `.properties`, subtitles
//! (SRT, WebVTT), calendars and contacts (iCalendar, vCard), Visual Studio solutions, G-code, Inno Setup scripts
//! and PPCL programs.

use super::code;
use super::{Lang, Out, State, Tok, at, find, is_num_word, line_end, scan_str};

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | 0x0C)
}

// ---- TOML ----

// What `State::kind` means here; `a` is 1 when an escape is pending.
/// `"""…"""`
const T_ML_BASIC: u8 = 1;
/// `'''…'''`
const T_ML_LITERAL: u8 = 2;
/// `"…"` cut off by the end of a text (they end at their line's end).
const T_BASIC: u8 = 3;
/// `'…'` cut off by the end of a text.
const T_LITERAL: u8 = 4;
/// A comment cut off by the end of a text.
const T_COMMENT: u8 = 5;

/// `b` holds the arrays and inline tables a value has open across lines: how many (bits 0-3), and whether each of the
/// first 12 is an inline table (bit 3 + its depth).
fn toml_push(b: u16, table: bool) -> u16 {
    let d = b & 15;
    if d == 15 {
        return b;
    }
    let bit = if d < 12 { 1 << (4 + d) } else { 0 };
    let r = (b & !15) | (d + 1);
    if table { r | bit } else { r & !bit }
}

fn toml_pop(b: u16) -> u16 {
    let d = b & 15;
    if d == 0 {
        return b;
    }
    let bit = if d <= 12 { 1 << (3 + d) } else { 0 };
    ((b & !15) | (d - 1)) & !bit
}

fn toml_in_table(b: u16) -> bool {
    let d = b & 15;
    d > 0 && d <= 12 && b & (1 << (3 + d)) != 0
}

/// A string from `s`, scanned from `i`, of the given kind: Ok(end), or the state to go on in.
fn toml_str(t: &[u8], s: usize, mut i: usize, kind: u8, mut esc: bool, o: &mut Out) -> Result<usize, (u8, bool)> {
    let (q, escapes, multi) = match kind {
        T_ML_BASIC => (b'"', true, true),
        T_ML_LITERAL => (b'\'', false, true),
        T_BASIC => (b'"', true, false),
        _ => (b'\'', false, false),
    };
    while i < t.len() {
        let c = t[i];
        if esc {
            esc = false;
        } else if c == b'\\' && escapes {
            esc = true;
        } else if c == b'\n' && !multi {
            o.put(s, i, Tok::Str);
            return Ok(i);
        } else if c == q && (!multi || (at(t, i + 1) == q && at(t, i + 2) == q)) {
            // """ ends it; up to two more quotes are part of the text (`""""a""""`)
            let e = if multi { i + t[i..].iter().take(5).take_while(|&&b| b == q).count() } else { i + 1 };
            o.put(s, e, Tok::Str);
            return Ok(e);
        }
        i += 1;
    }
    o.put(s, t.len(), Tok::Str);
    Err((kind, esc))
}

/// A `[table]` or `[[array.of.tables]]` header from `i`: its end (after the brackets, or the line's end).
fn toml_header_end(t: &[u8], i: usize) -> usize {
    let le = line_end(t, i);
    let double = at(t, i + 1) == b'[';
    let mut j = i + 1 + double as usize;
    while j < le {
        match t[j] {
            b'"' | b'\'' => {
                let q = t[j];
                let (e, _, _) = scan_str(&t[..le], j + 1, q, if q == b'"' { b'\\' } else { 0 }, false, true);
                j = e;
            }
            b']' => return (j + 1 + (double && at(t, j + 1) == b']') as usize).min(le),
            _ => j += 1,
        }
    }
    le
}

/// Inside an array left open (by mistake, as arrays don't hold tables): a `[name]` or `[[name]]` header from the
/// line's very start, which ends it. Only the header itself is looked at (no space in it), so it reads the same
/// wherever the text is cut.
fn toml_resync(t: &[u8], i: usize) -> bool {
    let d = 1 + (at(t, i + 1) == b'[') as usize;
    let first = at(t, i + d);
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    let e = i + d + t[i + d..].iter().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')).count();
    t[e..].iter().take(d).all(|&b| b == b']') && matches!(at(t, e + d), 0 | b' ' | b'\t' | b'\r' | b'\n' | b'#')
}

/// A key starting at `i` (`name`, `a.b`, `"quoted"`, `site."google.com"`) that an `=` follows on its line: where the
/// `=` is. (Keys are short: it looks 1 KB ahead at most.)
fn toml_key_eq(t: &[u8], mut i: usize) -> Option<usize> {
    let le = t.len().min(i + 1024);
    let spaces = |i: usize| i + t[i.min(le)..le].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
    loop {
        match t.get(i).filter(|_| i < le) {
            Some(&q @ (b'"' | b'\'')) => {
                let mut j = i + 1;
                while j < le && t[j] != q && t[j] != b'\n' {
                    j += if t[j] == b'\\' && q == b'"' { 2 } else { 1 };
                }
                if j >= le || t[j] != q {
                    return None;
                }
                i = j + 1;
            }
            Some(&c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                i += t[i..le].iter().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')).count();
            }
            _ => return None,
        }
        i = spaces(i);
        match t.get(i).filter(|_| i < le) {
            Some(b'.') => i = spaces(i + 1),
            Some(b'=') => return Some(i),
            _ => return None,
        }
    }
}

pub(super) fn toml(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut stack = st.b;
    let mut i = 0;
    match st.kind {
        T_ML_BASIC | T_ML_LITERAL | T_BASIC | T_LITERAL => match toml_str(t, 0, 0, st.kind, st.a & 1 != 0, o) {
            Ok(e) => i = e,
            Err((kind, esc)) => return State { kind, a: esc as u8, ..st },
        },
        T_COMMENT => {
            i = line_end(t, 0);
            o.put(0, i, Tok::Comment);
            if i == n {
                return st;
            }
        }
        _ => {}
    }
    let (mut bol, mut col0) = (st.bol && i == 0, st.col0 && i == 0);
    // after `{` or `,` in an inline table, where a key comes
    let mut key_pos = false;
    while i < n {
        let c = t[i];
        match c {
            b'\n' => {
                (bol, col0) = (true, true);
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                col0 = false;
                i += 1;
                continue;
            }
            _ => {}
        }
        let (line_start, at_col0, can_key) = (bol, col0, (bol && stack & 15 == 0) || key_pos);
        (bol, col0, key_pos) = (false, false, false);
        if c == b'[' && line_start && (stack & 15 == 0 || (at_col0 && toml_resync(t, i))) {
            stack = 0;
            let e = toml_header_end(t, i);
            o.put(i, e, Tok::Section);
            i = e;
            continue;
        }
        if c == b'#' {
            let e = line_end(t, i);
            o.put(i, e, Tok::Comment);
            if e == n {
                return State { kind: T_COMMENT, a: 0, b: stack, ..st };
            }
            i = e;
            continue;
        }
        if can_key {
            if let Some(eq) = toml_key_eq(t, i) {
                o.put(i, t[i..eq].trim_ascii_end().len() + i, Tok::Key);
                o.put(eq, eq + 1, Tok::Punct);
                i = eq + 1;
                continue;
            }
        }
        match c {
            b'"' | b'\'' => {
                let triple = at(t, i + 1) == c && at(t, i + 2) == c;
                let kind = match (c, triple) {
                    (b'"', true) => T_ML_BASIC,
                    (b'"', false) => T_BASIC,
                    (_, true) => T_ML_LITERAL,
                    _ => T_LITERAL,
                };
                match toml_str(t, i, i + if triple { 3 } else { 1 }, kind, false, o) {
                    Ok(e) => i = e,
                    Err((kind, esc)) => return State { kind, a: esc as u8, b: stack, ..st },
                }
            }
            b'[' | b'{' => {
                o.put(i, i + 1, Tok::Punct);
                stack = toml_push(stack, c == b'{');
                key_pos = c == b'{';
                i += 1;
            }
            b']' | b'}' => {
                o.put(i, i + 1, Tok::Punct);
                stack = toml_pop(stack);
                i += 1;
            }
            b',' | b'=' => {
                o.put(i, i + 1, Tok::Punct);
                key_pos = c == b',' && toml_in_table(stack);
                i += 1;
            }
            c if c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'_' | b'.') => {
                let word = |i: usize| i + t[i..].iter().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'+' | b'-')).count();
                let mut e = word(i);
                let w = &t[i..e];
                let tok = match w {
                    b"true" | b"false" => Some(Tok::Lit),
                    b"inf" | b"+inf" | b"-inf" | b"nan" | b"+nan" | b"-nan" => Some(Tok::Num),
                    _ if is_num_word(w) => {
                        // a date and a time with a space between: 1979-05-27 07:32:00
                        if w.len() == 10 && w[4] == b'-' && at(t, e) == b' ' && at(t, e + 1).is_ascii_digit() && at(t, e + 3) == b':' {
                            e = word(e + 1);
                        }
                        Some(Tok::Num)
                    }
                    _ => None,
                };
                if let Some(tok) = tok {
                    o.put(i, e, tok);
                }
                i = e.max(i + 1);
            }
            _ => i += 1,
        }
    }
    State { kind: 0, a: 0, b: stack, ..st }
}

// ---- nginx ----

/// A quoted string cut off by the end of a text; `b` is its quote, and `a` bit 2 an escape pending.
const N_STR: u8 = 1;
/// A comment cut off by the end of a text.
const N_COMMENT: u8 = 2;

/// nginx's blocks (the others are directives).
const NGINX_BLOCKS: [&[u8]; 13] = [
    b"events", b"geo", b"http", b"if", b"limit_except", b"location", b"mail", b"map", b"server", b"split_clients",
    b"stream", b"types", b"upstream",
];

/// nginx: `name args;` and `name args { … }`. `a`: bit 0 inside a statement (its name is behind), bit 1 the last
/// byte was part of a word (where `#` doesn't start a comment).
pub(super) fn nginx(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut i = 0;
    let (mut in_stmt, mut in_word) = (st.a & 1 != 0, st.a & 2 != 0);
    match st.kind {
        N_STR => {
            let q = st.b as u8;
            let (e, closed, esc) = scan_str(t, 0, q, b'\\', st.a & 4 != 0, true);
            o.put(0, e, Tok::Str);
            if !closed {
                return State { kind: N_STR, a: (st.a & 3) | (esc as u8) << 2, ..st };
            }
            i = e;
        }
        N_COMMENT => {
            i = line_end(t, 0);
            o.put(0, i, Tok::Comment);
            if i == n {
                return st;
            }
        }
        _ => {}
    }
    while i < n {
        let c = t[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                in_word = false;
                i += 1;
            }
            b';' | b'{' | b'}' => {
                o.put(i, i + 1, Tok::Punct);
                (in_stmt, in_word) = (false, false);
                i += 1;
            }
            b'#' if !in_word => {
                let e = line_end(t, i);
                o.put(i, e, Tok::Comment);
                if e == n {
                    return State { kind: N_COMMENT, a: in_stmt as u8, b: 0, ..st };
                }
                i = e;
            }
            b'"' | b'\'' => {
                let (e, closed, esc) = scan_str(t, i + 1, c, b'\\', false, true);
                o.put(i, e, Tok::Str);
                (in_stmt, in_word) = (true, true);
                if !closed {
                    return State { kind: N_STR, a: 3 | (esc as u8) << 2, b: c as u16, ..st };
                }
                i = e;
            }
            b'$' if at(t, i + 1) == b'{' || at(t, i + 1).is_ascii_alphanumeric() || at(t, i + 1) == b'_' => {
                // $host, ${name}
                let braced = at(t, i + 1) == b'{';
                let from = i + 1 + braced as usize;
                let mut e = from + t[from..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
                if braced && at(t, e) == b'}' {
                    e += 1;
                }
                o.put(i, e, Tok::Var);
                (in_stmt, in_word) = (true, true);
                i = e;
            }
            _ => {
                let e = i + t[i..].iter().take_while(|&&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b';' | b'{' | b'}' | b'"' | b'\'' | b'$')).count();
                let e = e.max(i + 1);
                let w = &t[i..e];
                let tok = if !in_stmt && !in_word {
                    in_stmt = true;
                    Some(if NGINX_BLOCKS.contains(&w) { Tok::Control } else { Tok::Keyword })
                } else if in_word {
                    None
                } else if w == b"on" || w == b"off" {
                    Some(Tok::Lit)
                } else if w[0].is_ascii_digit() && is_num_word(w) {
                    Some(Tok::Num)
                } else {
                    None
                };
                if let Some(tok) = tok {
                    o.put(i, e, tok);
                }
                in_word = true;
                i = e;
            }
        }
    }
    State { kind: 0, a: in_stmt as u8 | (in_word as u8) << 1, b: 0, ..st }
}

// ---- Apache ----

/// A `"string"` cut off by the end of a text; `a` bit 1: an escape pending.
const A_STR: u8 = 1;
/// A comment cut off by the end of a text.
const A_COMMENT: u8 = 2;

/// Apache's configuration and `.htaccess` files: a directive per line (`a` bit 0: this line goes on from the one
/// before, which ended with `\`), `<Section args>` … `</Section>`, `#` comments on their own lines.
pub(super) fn apache(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut i = 0;
    let mut cont = st.a & 1 != 0;
    if st.kind == A_STR {
        let (e, closed, esc) = scan_str(t, 0, b'"', b'\\', st.a & 2 != 0, true);
        o.put(0, e, Tok::Str);
        if !closed {
            return State { kind: A_STR, a: cont as u8 | (esc as u8) << 1, ..st };
        }
        i = e;
    } else if st.kind == A_COMMENT {
        i = line_end(t, 0);
        o.put(0, i, Tok::Comment);
        if i == n {
            return st;
        }
    }
    let mut bol = st.bol && i == 0;
    // inside `<Section …>` (colored up to its `>`)
    let mut section = false;
    while i < n {
        let c = t[i];
        match c {
            b'\n' => {
                let k = if i > 0 && t[i - 1] == b'\r' { i - 1 } else { i };
                cont = k > 0 && t[k - 1] == b'\\';
                (bol, section) = (true, false);
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let first = bol && !cont;
        bol = false;
        if first && c == b'#' {
            let e = line_end(t, i);
            o.put(i, e, Tok::Comment);
            if e == n {
                return State { kind: A_COMMENT, a: cont as u8, b: 0, ..st };
            }
            i = e;
            continue;
        }
        if first && c == b'<' {
            let ns = i + 1 + (at(t, i + 1) == b'/') as usize;
            let ne = ns + t[ns..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
            o.put(i, ns, Tok::Punct);
            o.put(ns, ne, Tok::Tag);
            section = true;
            i = ne;
            continue;
        }
        match c {
            b'>' if section => {
                o.put(i, i + 1, Tok::Punct);
                section = false;
                i += 1;
            }
            b'"' => {
                let (e, closed, esc) = scan_str(t, i + 1, b'"', b'\\', false, true);
                o.put(i, e, Tok::Str);
                if !closed {
                    return State { kind: A_STR, a: cont as u8 | (esc as u8) << 1, b: 0, ..st };
                }
                i = e;
            }
            // %{HTTP_HOST}, ${APACHE_LOG_DIR}, $1, %1
            b'%' | b'$' if at(t, i + 1) == b'{' || at(t, i + 1).is_ascii_digit() => {
                let e = if at(t, i + 1) == b'{' {
                    let k = t[i + 2..].iter().take(128).take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-' | b'.')).count();
                    if at(t, i + 2 + k) == b'}' { i + 3 + k } else { i + 1 }
                } else {
                    i + 2
                };
                if e > i + 1 {
                    o.put(i, e, Tok::Var);
                }
                i = e;
            }
            // RewriteRule flags: [L,R=301,NC]
            b'[' if i > 0 && is_ws(t[i - 1]) => {
                let k = t[i + 1..].iter().take(128).take_while(|&&b| !matches!(b, b']' | b' ' | b'\t' | b'\r' | b'\n' | b'"')).count();
                if at(t, i + 1 + k) == b']' {
                    o.put(i, i + k + 2, Tok::Attr);
                    i += k + 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                let e = i + t[i..]
                    .iter()
                    .take_while(|&&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'%' | b'$') && !(section && b == b'>'))
                    .count();
                let e = e.max(i + 1);
                let w = &t[i..e];
                let tok = if first {
                    Some(Tok::Keyword)
                } else if section {
                    Some(Tok::Attr)
                } else if w.eq_ignore_ascii_case(b"on") || w.eq_ignore_ascii_case(b"off") {
                    Some(Tok::Lit)
                } else if w[0].is_ascii_digit() && is_num_word(w) {
                    Some(Tok::Num)
                } else {
                    None
                };
                if let Some(tok) = tok {
                    o.put(i, e, tok);
                }
                i = e;
            }
        }
    }
    State { kind: 0, a: cont as u8, b: 0, ..st }
}

// ---- Java properties ----

// Where a `key = value` line is, in `a` (bits 0-2); bit 6: a continued line's leading whitespace is being skipped,
// bit 7: an escape is pending.
const P_START: u8 = 0;
const P_KEY: u8 = 1;
/// After the key: whitespace, then one `=` or `:`.
const P_SEP: u8 = 2;
const P_VALUE: u8 = 3;
const P_COMMENT: u8 = 4;

/// `.properties` files: `key=value`, `key: value` or `key value`; `#` and `!` comment lines; a line ending in an odd
/// number of backslashes goes on in the next one (which isn't a new key, nor a comment).
pub(super) fn properties(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut phase = st.a & 7;
    let (mut skip, mut esc) = (st.a & 0x40 != 0, st.a & 0x80 != 0);
    let mut i = 0;
    let mut key_start = 0;
    while i < n {
        let c = t[i];
        if c == b'\n' {
            if phase == P_KEY {
                o.put(key_start, i, Tok::Key);
            }
            if esc && phase != P_COMMENT {
                skip = true;
            } else {
                (phase, skip) = (P_START, false);
            }
            esc = false;
            i += 1;
            continue;
        }
        if skip {
            if is_ws(c) {
                i += 1;
                continue;
            }
            skip = false;
            if phase == P_KEY {
                key_start = i;
            }
        }
        if esc {
            // \uXXXX, \n, \=, `\` + CRLF
            esc = c == b'\r';
            let e = if c == b'u' { i + 1 + t[i + 1..].iter().take(4).take_while(|b| b.is_ascii_hexdigit()).count() } else { i + 1 };
            if !esc && phase != P_KEY {
                o.put(i.saturating_sub(1), e, Tok::Lit);
            }
            i = e;
            continue;
        }
        match phase {
            P_START => {
                if is_ws(c) {
                    i += 1;
                } else if c == b'#' || c == b'!' {
                    phase = P_COMMENT;
                } else {
                    (phase, key_start) = (P_KEY, i);
                }
            }
            P_KEY => {
                if c == b'\\' {
                    esc = true;
                    i += 1;
                } else if c == b'=' || c == b':' || is_ws(c) {
                    o.put(key_start, i, Tok::Key);
                    phase = P_SEP;
                } else {
                    i += 1;
                }
            }
            P_SEP => {
                if c == b'=' || c == b':' {
                    o.put(i, i + 1, Tok::Punct);
                    phase = P_VALUE;
                    i += 1;
                } else if is_ws(c) {
                    i += 1;
                } else {
                    phase = P_VALUE;
                }
            }
            P_COMMENT => {
                let e = line_end(t, i);
                o.put(i, e, Tok::Comment);
                i = e;
            }
            _ => {
                if c == b'\\' {
                    esc = true;
                } else if c == b'$' && at(t, i + 1) == b'{' {
                    // ${placeholder} (Spring and others)
                    let k = t[i + 2..].iter().take(128).take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b':')).count();
                    if at(t, i + 2 + k) == b'}' {
                        o.put(i, i + k + 3, Tok::Var);
                        i += k + 3;
                        continue;
                    }
                }
                i += 1;
            }
        }
    }
    if phase == P_KEY && !skip {
        o.put(key_start, n, Tok::Key);
    }
    State { kind: 0, a: phase | (skip as u8) << 6 | (esc as u8) << 7, b: 0, ..st }
}

// ---- subtitles ----

/// Inside a WebVTT `NOTE`: a comment up to a blank line.
const S_NOTE: u8 = 1;
/// In `State::mode`: inside a WebVTT `STYLE` block (CSS up to a blank line), whose CSS state `kind`, `a` and `b` are.
const S_STYLE: u8 = 1;

/// SRT and WebVTT: cue numbers, `00:00:01,000 --> 00:00:04,000` timings (with WebVTT's settings after them), tags in
/// the text (`<i>`, `<v Bob>`, `<c.yellow>`), WebVTT's header, `NOTE` comments and `STYLE` blocks.
pub(super) fn subtitles(t: &[u8], mut st: State, o: &mut Out) -> State {
    let mut start = 0;
    let mut col0 = st.col0;
    let mut bol = st.bol;
    loop {
        let end = line_end(t, start);
        let nl = end < t.len();
        let line = &t[start..end];
        let blank = (col0 || bol) && line.iter().all(|&b| is_ws(b));
        let mut sub = o.at(start);
        if st.mode == S_STYLE {
            if blank && nl {
                st = State { kind: 0, a: 0, b: 0, mode: 0, ..st };
            } else {
                let s = code::css(&t[start..end + nl as usize], State { mode: 0, ..st }, &mut sub);
                st = State { mode: S_STYLE, ..s };
            }
        } else if st.kind == S_NOTE {
            if blank && nl {
                st.kind = 0;
            } else {
                sub.put(0, line.len(), Tok::Comment);
            }
        } else {
            st = sub_line(line, col0, st, &mut sub);
        }
        if !nl {
            return st;
        }
        start = end + 1;
        (col0, bol) = (true, true);
    }
}

fn sub_line(l: &[u8], col0: bool, mut st: State, o: &mut Out) -> State {
    let n = l.len();
    let tl = l.trim_ascii_end();
    if col0 {
        let word = |w: &[u8]| tl.starts_with(w) && matches!(at(tl, w.len()), 0 | b' ' | b'\t');
        if word(b"WEBVTT") {
            o.put(0, 6, Tok::Keyword);
            o.put(6, n, Tok::Dim);
            return st;
        }
        if word(b"NOTE") {
            o.put(0, n, Tok::Comment);
            st.kind = S_NOTE;
            return st;
        }
        if tl == b"STYLE" {
            o.put(0, n, Tok::Keyword);
            return State { kind: 0, a: 0, b: 0, mode: S_STYLE, ..st };
        }
        if tl == b"REGION" {
            o.put(0, n, Tok::Keyword);
            return st;
        }
        if !tl.is_empty() && tl.len() < 10 && tl.iter().all(u8::is_ascii_digit) {
            o.put(0, tl.len(), Tok::Num);
            return st;
        }
        if let Some(p) = tl.windows(3).position(|w| w == b"-->") {
            // 00:00:01,000 --> 00:00:04,000 align:start position:10%
            let stamp = |o: &mut Out, a: usize, b: usize| {
                let s = a + l[a..b].iter().take_while(|&&b| b == b' ').count();
                o.put(s, b, Tok::Num);
            };
            stamp(o, 0, l[..p].trim_ascii_end().len());
            o.put(p, p + 3, Tok::Punct);
            let s = p + 3 + l[p + 3..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
            let e = s + l[s..].iter().take_while(|&&b| !is_ws(b)).count();
            stamp(o, s, e);
            let mut i = e;
            while i < n {
                let w = i + l[i..].iter().take_while(|&&b| !is_ws(b)).count();
                if let Some(c) = l[i..w].iter().position(|&b| b == b':') {
                    o.put(i, i + c, Tok::Attr);
                }
                i = w + 1;
            }
            return st;
        }
    }
    // cue text: <i>, </b>, <font color="…">, <v Bob>, <00:00:01.500>, {\an8}, &amp;
    let mut i = 0;
    while i < n {
        match l[i] {
            b'<' => match memchr::memchr2(b'>', b'<', &l[i + 1..n.min(i + 257)]) {
                Some(p) if p > 0 && l[i + 1 + p] == b'>' => {
                    o.put(i, i + p + 2, Tok::Tag);
                    i += p + 2;
                }
                _ => i += 1,
            },
            b'{' if at(l, i + 1) == b'\\' => match l[i..].iter().take(64).position(|&b| b == b'}') {
                Some(p) => {
                    o.put(i, i + p + 1, Tok::Dim);
                    i += p + 1;
                }
                None => i += 1,
            },
            b'&' => {
                let k = l[i + 1..].iter().take(10).take_while(|b| b.is_ascii_alphanumeric() || **b == b'#').count();
                if k > 0 && at(l, i + 1 + k) == b';' {
                    o.put(i, i + k + 2, Tok::Lit);
                    i += k + 2;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    st
}

// ---- iCalendar and vCard ----

/// `NAME;PARAM=value;PARAM="quoted":value` lines; `BEGIN:VEVENT` / `END:VEVENT` as sections. A line starting with a
/// space or a tab goes on with the value of the line before (folding), so it stays plain.
pub(super) fn calendar_line(l: &[u8], col0: bool, _bol: bool, o: &mut Out) {
    if !col0 || l.is_empty() || is_ws(l[0]) {
        return;
    }
    let n = l.len();
    let name_end = l.iter().position(|&b| b == b';' || b == b':').unwrap_or(n);
    let name = &l[..name_end];
    if name.eq_ignore_ascii_case(b"BEGIN") || name.eq_ignore_ascii_case(b"END") {
        o.put(0, l.trim_ascii_end().len(), Tok::Section);
        return;
    }
    o.put(0, name_end, Tok::Key);
    let mut i = name_end;
    while at(l, i) == b';' {
        o.put(i, i + 1, Tok::Punct);
        i += 1;
        let e = i + l[i..].iter().take_while(|&&b| !matches!(b, b'=' | b';' | b':')).count();
        o.put(i, e, Tok::Attr);
        i = e;
        if at(l, i) != b'=' {
            continue;
        }
        o.put(i, i + 1, Tok::Punct);
        i += 1;
        loop {
            if at(l, i) == b'"' {
                let e = l[i + 1..].iter().position(|&b| b == b'"').map_or(n, |p| i + p + 2);
                o.put(i, e, Tok::Str);
                i = e;
            } else {
                i += l[i..].iter().take_while(|&&b| !matches!(b, b',' | b';' | b':')).count();
            }
            if at(l, i) != b',' {
                break;
            }
            i += 1;
        }
    }
    if at(l, i) == b':' {
        o.put(i, i + 1, Tok::Punct);
        let v = l[i + 1..].trim_ascii_end();
        let e = i + 1 + v.len();
        if is_num_word(v) {
            o.put(i + 1, e, Tok::Num);
        } else if v.eq_ignore_ascii_case(b"TRUE") || v.eq_ignore_ascii_case(b"FALSE") {
            o.put(i + 1, e, Tok::Lit);
        } else if v.len() > 7 && (v[..7].eq_ignore_ascii_case(b"mailto:") || v.starts_with(b"http")) {
            o.put(i + 1, e, Tok::Link);
        }
    }
}

// ---- Visual Studio solutions ----

const SLN_WORDS: [&[u8]; 8] = [
    b"EndGlobal", b"EndGlobalSection", b"EndProject", b"EndProjectSection", b"Global", b"GlobalSection", b"Project",
    b"ProjectSection",
];

/// A `.sln` line: `Project("{…}") = "Name", "Name\Name.csproj", "{…}"`, `GlobalSection(…) = preSolution`,
/// `{…}.Debug|Any CPU.ActiveCfg = Debug|Any CPU`, `VisualStudioVersion = 17.0.31903.59`.
pub(super) fn sln_line(l: &[u8], _col0: bool, bol: bool, o: &mut Out) {
    if !bol {
        return;
    }
    let n = l.len();
    let s = l.iter().position(|&b| !is_ws(b)).unwrap_or(n);
    let rest = &l[s..];
    if rest.starts_with(b"#") {
        o.put(s, n, Tok::Comment);
        return;
    }
    if rest.starts_with(b"Microsoft Visual Studio Solution File") {
        o.put(s, n, Tok::Heading);
        return;
    }
    let w = s + rest.iter().take_while(|b| b.is_ascii_alphanumeric()).count();
    let keyword = SLN_WORDS.contains(&&l[s..w]);
    let mut i = s;
    if keyword {
        o.put(s, w, Tok::Keyword);
        i = w;
    }
    let eq = l[i..].windows(3).position(|x| x == b" = ").map(|p| i + p);
    let guid = |o: &mut Out, a: usize, b: usize, other: Option<Tok>| {
        // `{…}` GUIDs as numbers, the rest as `other`
        let mut k = a;
        while k < b {
            match l[k..b].iter().position(|&c| c == b'{') {
                Some(p) => {
                    if let Some(tok) = other {
                        o.put(k, k + p, tok);
                    }
                    let close = l[k + p..b].iter().take(64).position(|&c| c == b'}').map_or(k + p + 1, |q| k + p + q + 1);
                    o.put(k + p, close, Tok::Num);
                    k = close;
                }
                None => {
                    if let Some(tok) = other {
                        o.put(k, b, tok);
                    }
                    k = b;
                }
            }
        }
    };
    if !keyword {
        // `key = value` inside a section
        if let Some(eq) = eq {
            guid(o, i, eq, Some(Tok::Key));
            o.put(eq + 1, eq + 2, Tok::Punct);
            let v = l[eq + 3..].trim_ascii_end();
            if is_num_word(v) {
                o.put(eq + 3, eq + 3 + v.len(), Tok::Num);
            } else {
                guid(o, eq + 3, eq + 3 + v.len(), None);
            }
        }
        return;
    }
    // Project("{…}") = "Name", "path", "{…}"   GlobalSection(Name) = preSolution
    while i < n {
        match l[i] {
            b'"' => {
                let e = l[i + 1..].iter().position(|&b| b == b'"').map_or(n, |p| i + p + 2);
                if at(l, i + 1) == b'{' {
                    o.put(i, e, Tok::Num);
                } else {
                    o.put(i, e, Tok::Str);
                }
                i = e;
            }
            b'(' => {
                let e = l[i..].iter().take(256).position(|&b| b == b')').map_or(i + 1, |p| i + p);
                if at(l, i + 1) != b'"' {
                    o.put(i + 1, e, Tok::Type);
                    i = e.max(i + 1);
                } else {
                    i += 1;
                }
            }
            b'=' => {
                o.put(i, i + 1, Tok::Punct);
                i += 1;
            }
            c if c.is_ascii_alphabetic() => {
                let e = i + l[i..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
                if matches!(&l[i..e], b"preSolution" | b"postSolution" | b"preProject" | b"postProject") {
                    o.put(i, e, Tok::Lit);
                }
                i = e;
            }
            _ => i += 1,
        }
    }
}

// ---- PPCL ----

// What `State::kind` means here, before a line's statement starts (in a statement it's 0); `b` holds the line number
// before (0: none), so a number out of order shows.
/// After the `#` that turns a line off, before its number.
const PC_HASH: u8 = 1;
/// After the line number, before the statement: where a `C` makes the line a comment.
const PC_LEAD: u8 = 2;
const PC_COMMENT: u8 = 3;
/// The rest of a line that's turned off.
const PC_OFF: u8 = 4;

/// PPCL's commands and functions, and four that panels' firmware has but no manual does (`DIM`, `ENTHAL`, `ONERR`,
/// `RELTCU`). Sorted.
pub(super) const PPCL_COMMANDS: [&[u8]; 81] = [
    b"ACT", b"ADAPTM", b"ADAPTS", b"ALARM", b"ALMPRI", b"ATN", b"AUTO", b"COM", b"COS", b"DAY", b"DBSWIT", b"DC", b"DCR",
    b"DEACT", b"DEFINE", b"DIM", b"DISABL", b"DISALM", b"DISCOV", b"DPHONE", b"EMAUTO", b"EMFAST", b"EMOFF", b"EMON",
    b"EMSET", b"EMSLOW", b"ENABLE", b"ENALM", b"ENCOV", b"ENTHAL", b"EPHONE", b"EXP", b"FAST", b"GETVAL", b"GOSUB",
    b"GOTO", b"HLIMIT", b"HOLIDA", b"INITTO", b"LLIMIT", b"LN", b"LOCAL", b"LOG", b"LOOP", b"LSQ2", b"LSQDAT", b"LSTSQR",
    b"MAX", b"MIN", b"NIGHT", b"NORMAL", b"OFF", b"OIP", b"ON", b"ONERR", b"ONPWRT", b"PDL", b"PDLDAT", b"PDLDPG",
    b"PDLMTR", b"PDLSET", b"RELEAS", b"RELTCU", b"RETURN", b"SAMPLE", b"SET", b"SETVAL", b"SIN", b"SLOW", b"SQRT",
    b"SSTO", b"SSTOCO", b"STATE", b"TABLE", b"TAN", b"TIMAVG", b"TOD", b"TODMOD", b"TODSET", b"TOTAL", b"WAIT",
];

/// The words statements are made of (besides `C`, which starts a comment). Sorted.
const PPCL_WORDS: [&[u8]; 7] = [b"ELSE", b"GOSUB", b"GOTO", b"IF", b"PARAMETER", b"RETURN", b"THEN"];

/// Values: the states points are compared with, and the resident points (the time, the day, alarm counts...); also
/// `NODE0`…`NODE99`, `SECND1`…`SECND7` and every `$` name (`$LOC1`, `$ARG1`, `$BATT`...). Sorted.
const PPCL_VALUES: [&[u8]; 25] = [
    b"ALARM", b"ALMACK", b"ALMCNT", b"ALMCT2", b"AUTO", b"CRTIME", b"DAY", b"DAYMOD", b"DAYOFM", b"DEAD", b"FAILED",
    b"FAST", b"HAND", b"LINK", b"LOW", b"MONTH", b"NGTMOD", b"OFF", b"OK", b"ON", b"PRFON", b"SECNDS", b"SLOW", b"TIME",
    b"TROUBL",
];

/// The five priorities a program can name (`@OPER`...). Sorted.
const PPCL_PRIORITIES: [&[u8]; 5] = [b"EMER", b"NONE", b"OPER", b"PDL", b"SMOKE"];

/// The dotted operators, all there are (`.NOT.` isn't one). Sorted.
const PPCL_OPERATORS: [&[u8]; 11] =
    [b"AND", b"EQ", b"GE", b"GT", b"LE", b"LT", b"NAND", b"NE", b"OR", b"ROOT", b"XOR"];

/// Whether `w` is in `list` (sorted, in capitals), in any case.
pub(super) fn ppcl_in(list: &[&[u8]], w: &[u8]) -> bool {
    let mut up = [0u8; 12];
    if w.is_empty() || w.len() > up.len() {
        return false;
    }
    for (u, c) in up.iter_mut().zip(w) {
        *u = c.to_ascii_uppercase();
    }
    list.binary_search(&&up[..w.len()]).is_ok()
}

fn ppcl_value(w: &[u8]) -> bool {
    let numbered = |p: &[u8]| {
        w.len() > p.len() && w[..p.len()].eq_ignore_ascii_case(p) && w[p.len()..].iter().all(u8::is_ascii_digit)
    };
    ppcl_in(&PPCL_VALUES, w) || numbered(b"NODE") || numbered(b"SECND")
}

/// The dotted operator at `i` (its `.`), if one is there: its length.
fn ppcl_operator(l: &[u8], i: usize) -> Option<usize> {
    let w = l.get(i + 1..)?.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    (at(l, i + 1 + w) == b'.' && ppcl_in(&PPCL_OPERATORS, &l[i + 1..i + 1 + w])).then_some(w + 2)
}

/// Whether a statement has a dotted operator in it.
pub(super) fn ppcl_has_operator(s: &[u8]) -> bool {
    (0..s.len()).any(|i| s[i] == b'.' && ppcl_operator(s, i).is_some())
}

/// PPCL programs (Siemens APOGEE and Desigo field panels): a number and one statement on each line, `00020     IF(
/// "AHU1.SAT" .GT. 55.0) THEN ON("AHU1.CLG")`. Colored much as Desigo shows them: the line number dimmed; a `C` as
/// the first word makes the line a comment; commands and `IF`, `THEN`, `ELSE`, `GOTO` as keywords; points by name
/// (`%X%` abbreviations marked); states, priorities, `$` locals and resident points (`ON`, `FAILED`, `@OPER`, `$LOC1`,
/// `TIME`) as values; the dotted operators plain; a line turned off (`# 00100 …`) dimmed. In red, what can't be right:
/// a line number out of order, a string left open, a `GOTO` without a line number, an operator or a priority PPCL
/// doesn't have, a name that needs quotes, and a statement the compiler couldn't read (`UNKNOWN (…)`).
pub(super) fn ppcl(t: &[u8], st: State, o: &mut Out) -> State {
    let (mut kind, mut prev, mut bol) = (st.kind, st.b, st.bol);
    let mut start = 0;
    loop {
        let end = line_end(t, start);
        let line = &t[start..end];
        let blank = kind == 0 && bol && line.iter().all(|&b| is_ws(b));
        (kind, prev) = ppcl_line(line, kind, prev, bol, &mut o.at(start));
        if end >= t.len() {
            break;
        }
        // (a blank line ends a program: the next one's numbers start over)
        if blank {
            prev = 0;
        }
        start = end + 1;
        (kind, bol) = (0, true);
    }
    State { kind, a: 0, b: prev, ..st }
}

/// One line, or what a text has of it: its start (which a text can end in anywhere a space is), then the statement.
/// Returns `kind` and the last line number at its end.
fn ppcl_line(l: &[u8], mut kind: u8, mut prev: u16, bol: bool, o: &mut Out) -> (u8, u16) {
    let n = l.len();
    let ws = |i: usize| i + l[i..].iter().take_while(|&&b| is_ws(b)).count();
    let mut i = 0;
    // Only a program's own lines get red marks, not a heading between programs or a line a statement goes on to
    // (with no number). (A text that starts after the number had one.)
    let mut numbered = true;
    if kind == 0 && bol {
        i = ws(0);
        if i == n {
            return (0, prev);
        }
        if l[i] == b'#' {
            o.put(i, i + 1, Tok::Dim);
            (kind, i) = (PC_HASH, i + 1);
        } else {
            kind = PC_LEAD;
            let s = i;
            (i, prev) = ppcl_number(l, i, prev, o);
            numbered = i > s;
        }
    }
    if kind == PC_HASH {
        i = ws(i);
        if i == n {
            return (kind, prev);
        }
        (i, prev) = ppcl_number(l, i, prev, o);
        o.put(i, n, Tok::Dim);
        return (PC_OFF, prev);
    }
    if kind == PC_LEAD {
        i = ws(i);
        if i == n {
            return (kind, prev);
        }
        kind = if matches!(l[i], b'C' | b'c') && (i + 1 == n || is_ws(l[i + 1])) { PC_COMMENT } else { 0 };
    }
    match kind {
        PC_COMMENT => o.put(i, n, Tok::Comment),
        PC_OFF => o.put(i, n, Tok::Dim),
        _ if o.on() => ppcl_statement(l, i, numbered, o),
        _ => {}
    }
    (kind, prev)
}

/// The line number at `i`, if there's one: dimmed, or red when it isn't 1…32767 or doesn't come after `prev` (the
/// line before's). Returns where it ends and the number to compare the next one with (`i` and 0 when there's none).
fn ppcl_number(l: &[u8], i: usize, prev: u16, o: &mut Out) -> (usize, u16) {
    let d = i + l[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    if d == i || !(d == l.len() || is_ws(l[d])) {
        return (i, 0);
    }
    let num = l[i..d].iter().fold(0u32, |v, &c| (v * 10 + (c - b'0') as u32).min(99_999));
    let valid = (1..=32767).contains(&num);
    o.put(i, d, if valid && num > prev as u32 { Tok::Dim } else { Tok::Error });
    (d, if valid { num as u16 } else { prev })
}

/// Where a number from `i` ends: `20`, `7.5`, `.5`, and times (`01:00`); a dot before a letter is an operator's.
fn ppcl_number_end(l: &[u8], i: usize) -> usize {
    let mut e = i + 1;
    while e < l.len() && (l[e].is_ascii_digit() || (matches!(l[e], b'.' | b':') && !at(l, e + 1).is_ascii_alphabetic())) {
        e += 1;
    }
    e
}

/// Where a word from `i` ends: letters and digits (its first character can be `$` or `@`), and dots too unless one
/// starts an operator: `ROOM.MIN.TEMP` is one word (a name that needs quotes), `A.ROOT.B` three.
fn ppcl_word_end(l: &[u8], i: usize) -> usize {
    let mut e = i + 1;
    loop {
        e += l[e..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
        if at(l, e) != b'.' || !at(l, e + 1).is_ascii_alphanumeric() || ppcl_operator(l, e).is_some() {
            return e;
        }
        e += 1;
    }
}

/// A point's name in quotes, from `s` to `e`, with its `%X%` abbreviations (from DEFINE) marked.
fn ppcl_name(l: &[u8], s: usize, e: usize, o: &mut Out) {
    let (mut from, mut j) = (s, s + 1);
    while j < e {
        if l[j] == b'%' {
            let w = l[j + 1..e].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
            if w > 0 && j + 1 + w < e && l[j + 1 + w] == b'%' {
                o.put(from, j, Tok::Var);
                o.put(j, j + w + 2, Tok::Type);
                j += w + 2;
                from = j;
                continue;
            }
        }
        j += 1;
    }
    o.put(from, e, Tok::Var);
}

/// Colors a statement from `i` to the line's end; with `checked`, what can't be right in red.
fn ppcl_statement(l: &[u8], mut i: usize, checked: bool, o: &mut Out) {
    #[derive(Clone, Copy, PartialEq)]
    enum Call {
        Define,
        Local,
        Oip,
    }
    let n = l.len();
    // inside DEFINE(…), LOCAL(…) or OIP(…), whose arguments aren't points: which, the depth of its `(`, the argument
    let mut call: Option<(Call, u32, u32)> = None;
    let mut depth = 0u32;
    let mut first = true;
    // (a mistake, or what it is on a line that isn't checked)
    let bad = |otherwise: Option<Tok>| if checked { Some(Tok::Error) } else { otherwise };
    while i < n {
        let (c, s) = (l[i], i);
        if is_ws(c) {
            i += 1;
            continue;
        }
        if c == b'"' {
            // (quoted text first: what's inside is one name, or OIP's keystrokes, whatever words it has)
            let close = l[i + 1..].iter().position(|&b| b == b'"');
            i = close.map_or(n, |p| i + p + 2);
            match call {
                _ if close.is_none() => {
                    if let Some(tok) = bad(None) {
                        o.put(s, i, tok);
                    }
                }
                // DEFINE's text and OIP's keystrokes, and the program's own locals (`"$TMR"`), stay plain
                Some((Call::Define | Call::Oip, ..)) => {}
                _ if at(l, s + 1) == b'$' => {}
                _ => ppcl_name(l, s, i, o),
            }
        } else if c.is_ascii_digit() || (c == b'.' && at(l, i + 1).is_ascii_digit()) {
            i = ppcl_number_end(l, i);
            o.put(s, i, Tok::Num);
        } else if c == b'.' {
            // `.EQ.` and the others stay plain; a word between dots that isn't one is a mistake (`.NOT.`, `.EQ`)
            let w = l[i + 1..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            match ppcl_operator(l, i) {
                Some(len) => i += len,
                None if w > 0 => {
                    i += 1 + w + (at(l, i + 1 + w) == b'.') as usize;
                    if let Some(tok) = bad(None) {
                        o.put(s, i, tok);
                    }
                }
                None => i += 1,
            }
        } else if c == b'%' && l[i + 1..].iter().take_while(|b| b.is_ascii_alphanumeric()).count() > 0 {
            // an abbreviation (`%X%`)
            let w = l[i + 1..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
            i += 1 + w + (at(l, i + 1 + w) == b'%') as usize;
            o.put(s, i, Tok::Type);
        } else if c.is_ascii_alphabetic() || (matches!(c, b'$' | b'@') && at(l, i + 1).is_ascii_alphanumeric()) {
            i = ppcl_word_end(l, i);
            let w = &l[s..i];
            let next = i + l[i..].iter().take_while(|&&b| is_ws(b)).count();
            let paren = at(l, next) == b'(';
            if first && checked && w.eq_ignore_ascii_case(b"UNKNOWN") {
                // a statement the compiler couldn't read, which the panel skips
                o.put(s, i, Tok::Error);
                o.put(i, n, Tok::Dim);
                return;
            }
            let is = |k: &[u8]| w.eq_ignore_ascii_case(k);
            let tok = if c == b'@' {
                if ppcl_in(&PPCL_PRIORITIES, &w[1..]) { Some(Tok::Str) } else { bad(Some(Tok::Str)) }
            } else if c == b'$' {
                Some(Tok::Str)
            } else if w.contains(&b'.') {
                bad(Some(Tok::Var))
            } else if paren {
                let known = ppcl_in(&PPCL_COMMANDS, w) || ppcl_in(&PPCL_WORDS, w);
                if call.is_none() {
                    let which = [(Call::Define, &b"DEFINE"[..]), (Call::Local, b"LOCAL"), (Call::Oip, b"OIP")];
                    call = which.iter().find(|x| is(x.1)).map(|x| (x.0, depth + 1, 0));
                }
                // (a command PPCL doesn't have stays plain)
                known.then_some(Tok::Keyword)
            } else if is(b"GOTO") || is(b"GOSUB") {
                // a line number has to follow
                let d = next + l[next..].iter().take_while(|b| b.is_ascii_digit()).count();
                let num = l[next..d].iter().fold(0u32, |v, &b| (v * 10 + (b - b'0') as u32).min(99_999));
                let ok = d > next && !(at(l, d).is_ascii_alphanumeric() || at(l, d) == b'.') && (1..=32767).contains(&num);
                o.put(s, i, if ok { Tok::Keyword } else { bad(Some(Tok::Keyword)).unwrap_or(Tok::Keyword) });
                if ok {
                    o.put(next, d, Tok::Num);
                    i = d;
                }
                first = false;
                continue;
            } else if ppcl_in(&PPCL_WORDS, w) {
                Some(Tok::Keyword)
            } else if ppcl_value(w) {
                Some(Tok::Str)
            } else if ppcl_in(&PPCL_COMMANDS, w) {
                Some(Tok::Keyword)
            } else {
                match call {
                    // the locals LOCAL declares, the abbreviation DEFINE gives a name
                    Some((Call::Local, ..)) => None,
                    Some((Call::Define, d, 0)) if depth == d => Some(Tok::Type),
                    // a point named without quotes, which only fit names of up to 6 letters and digits
                    _ if w.len() > 6 => bad(Some(Tok::Var)),
                    _ => Some(Tok::Var),
                }
            };
            if let Some(tok) = tok {
                o.put(s, i, tok);
            }
        } else {
            match c {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if call.is_some_and(|(_, d, _)| depth < d) {
                        call = None;
                    }
                }
                b',' => {
                    if let Some((_, d, arg)) = call.as_mut() {
                        if depth == *d {
                            *arg += 1;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        first = false;
    }
}

// ---- G-code ----

/// A line of G-code (3D printers, CNC machines): `N` line numbers, `G` and `M` codes, `T` tools, parameter words
/// (`X10.5`, `F1200`), `;` and `( … )` comments, `%` and `O` program numbers, `#` variables, `*` checksums,
/// Klipper's `COMMAND KEY=value` lines, the message of `M117` (and `M118`).
pub(super) fn gcode_line(l: &[u8], _col0: bool, bol: bool, o: &mut Out) {
    let n = l.len();
    let mut i = 0;
    // before the line's first word (a line number aside); after an `O` word (LinuxCNC's `o100 sub`); inside
    // `[…]` (an expression)
    let (mut first, mut after_o, mut depth) = (bol, false, 0u32);
    while i < n {
        let c = l[i];
        match c {
            b';' => {
                // a comment (Marlin, LinuxCNC…), or alone at the line's end Fanuc's end of block (whose comments are
                // in parentheses)
                let eob = l[i + 1..].iter().all(|&b| is_ws(b));
                o.put(i, n, if eob { Tok::Punct } else { Tok::Comment });
                return;
            }
            b'[' | b']' => {
                depth = if c == b'[' { depth + 1 } else { depth.saturating_sub(1) };
                i += 1;
            }
            b'(' => {
                let e = l[i..].iter().position(|&b| b == b')').map_or(n, |p| i + p + 1);
                o.put(i, e, Tok::Comment);
                i = e;
            }
            b'%' if first => {
                o.put(i, i + 1, Tok::Section);
                first = false;
                i += 1;
            }
            b'"' => {
                // RepRapFirmware's M550 P"name"
                let e = l[i + 1..].iter().position(|&b| b == b'"').map_or(n, |p| i + p + 2);
                o.put(i, e, Tok::Str);
                i = e;
            }
            b'#' => {
                // #100, #<_name>
                let e = if at(l, i + 1) == b'<' {
                    l[i..].iter().position(|&b| b == b'>').map_or(i + 1, |p| i + p + 1)
                } else {
                    i + 1 + l[i + 1..].iter().take_while(|b| b.is_ascii_digit()).count()
                };
                if e > i + 1 {
                    o.put(i, e, Tok::Var);
                }
                i = e;
            }
            // a checksum: N10 G1 X1*45
            b'*' if depth == 0 && at(l, i + 1).is_ascii_digit() => {
                let e = i + 1 + l[i + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                o.put(i, e, Tok::Dim);
                i = e;
            }
            c if c.is_ascii_alphabetic() => {
                let up = c.to_ascii_uppercase();
                let e = gcode_num_end(l, i + 1);
                if e > i + 1 {
                    match up {
                        b'G' => o.put(i, e, Tok::Keyword),
                        b'M' => o.put(i, e, Tok::Control),
                        b'T' => o.put(i, e, Tok::Type),
                        b'N' if first => o.put(i, e, Tok::Dim),
                        b'O' if first => o.put(i, e, Tok::Section),
                        _ => {
                            o.put(i, i + 1, Tok::Attr);
                            o.put(i + 1, e, Tok::Num);
                        }
                    }
                    let message = up == b'M' && matches!(&l[i + 1..e], b"117" | b"118");
                    after_o = up == b'O' && first;
                    first &= up == b'N';
                    i = e;
                    if message {
                        // its text, up to a comment
                        let s = i + l[i..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
                        let ce = memchr::memchr(b';', &l[s..]).map_or(n, |p| s + p);
                        o.put(s, ce, Tok::Str);
                        i = ce;
                    }
                    continue;
                }
                if first && up == b'O' && at(l, i + 1) == b'<' {
                    // o<name> sub
                    let e = l[i..].iter().position(|&b| b == b'>').map_or(n, |p| i + p + 1);
                    o.put(i, e, Tok::Section);
                    (first, after_o) = (false, true);
                    i = e;
                    continue;
                }
                // a word: Klipper's `SET_FAN_SPEED FAN=part SPEED=0.5`, LinuxCNC's `o100 sub`; `X#1`, `X[#2 * 2]`
                let e = i + l[i..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
                if at(l, e) == b'=' || (e == i + 1 && matches!(at(l, e), b'#' | b'[')) {
                    o.put(i, e, Tok::Attr);
                } else if first {
                    o.put(i, e, Tok::Func);
                } else if after_o {
                    o.put(i, e, Tok::Keyword);
                }
                (first, after_o) = (false, false);
                i = e;
            }
            c if c.is_ascii_digit() || (matches!(c, b'-' | b'+' | b'.') && gcode_num_end(l, i) > i) => {
                let e = gcode_num_end(l, i).max(i + 1);
                o.put(i, e, Tok::Num);
                i = e;
            }
            _ => i += 1,
        }
    }
}

/// Where a G-code number from `i` ends (`10`, `-1.5`, `.5`), or `i` when there is none.
fn gcode_num_end(l: &[u8], i: usize) -> usize {
    let s = i + matches!(at(l, i), b'+' | b'-') as usize;
    let d = l[s..].iter().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
    if l[s..s + d].iter().any(u8::is_ascii_digit) { s + d } else { i }
}

// ---- Inno Setup ----

/// In `State::kind`: inside the `[Code]` section, where the low bits (and `a`, `b`) are the Pascal lexer's state.
pub(super) const I_CODE: u8 = 0x80;
/// In `State::kind`: in a preprocessor line (`#define …`) cut off by the end of a text.
const I_PP: u8 = 0x40;

/// Inno Setup scripts (and its `.isl` message files): `[Section]` headers, `Name: value; Name: value` lines and
/// `Key=value` lines, `{app}`-style constants, `;` comment lines, the preprocessor's `#define …` lines and `{#Name}`;
/// the `[Code]` section is Pascal Script.
pub(super) fn inno(t: &[u8], mut st: State, o: &mut Out) -> State {
    let mut start = 0;
    let mut bol = st.bol;
    loop {
        let end = line_end(t, start);
        let nl = end < t.len();
        st = inno_line(&t[start..end + nl as usize], bol, st, &mut o.at(start));
        if !nl {
            return st;
        }
        start = end + 1;
        bol = true;
    }
}

/// One line of an Inno Setup script (`full`: with its line break, if any; `bol`: only whitespace before it).
fn inno_line(full: &[u8], bol: bool, st: State, o: &mut Out) -> State {
    let nl = full.last() == Some(&b'\n');
    let l = &full[..full.len() - nl as usize];
    // a preprocessor line, which may go on in the next text
    let pp = |st: State| State { kind: if nl { st.kind & !I_PP } else { st.kind | I_PP }, ..st };
    if st.kind & I_PP != 0 {
        if o.on() {
            ispp_line(l, 0, false, o);
        }
        return pp(st);
    }
    let s = if bol { l.iter().take_while(|&&b| is_ws(b)).count() } else { 0 };
    if bol && s < l.len() {
        // [Files], [Code] on a line of their own (in Pascal a line may start with a set: `[wpReady] then`)
        if l[s] == b'[' {
            let k = l[s + 1..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            let e = s + k + 2;
            if k > 0 && at(l, e - 1) == b']' && l[e..].iter().all(|&b| is_ws(b)) {
                o.put(s, e, Tok::Section);
                let code = l[s + 1..e - 1].eq_ignore_ascii_case(b"code");
                return State { kind: if code { I_CODE } else { 0 }, a: 0, b: 0, ..st };
            }
        }
        // #define, #include, #if … (anywhere, [Code] too)
        if l[s] == b'#' && at(l, s + 1).is_ascii_alphabetic() {
            if o.on() {
                ispp_line(l, s, true, o);
            }
            return pp(st);
        }
    }
    inno_body(full, bol, st, o)
}

/// A line of the current section, or what is left of it.
fn inno_body(full: &[u8], bol: bool, st: State, o: &mut Out) -> State {
    if st.kind & I_CODE != 0 {
        let s = code::code(code::syntax(Lang::InnoSetup), full, State { kind: st.kind & !I_CODE, col0: bol, bol, ..st }, o);
        return State { kind: s.kind | I_CODE, a: s.a, b: s.b, ..st };
    }
    // (the other sections' lines leave nothing open)
    if !o.on() {
        return st;
    }
    let l = full.strip_suffix(b"\n").unwrap_or(full);
    let n = l.len();
    if !bol {
        inno_value(l, 0, n, o);
        return st;
    }
    let s = l.iter().take_while(|&&b| is_ws(b)).count();
    if s == n {
        return st;
    }
    if l[s] == b';' || l[s..].starts_with(b"//") {
        o.put(s, n, Tok::Comment);
        return st;
    }
    let w = s + l[s..].iter().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')).count();
    let p = w + l[w..].iter().take_while(|&&b| is_ws(b)).count();
    match at(l, p) {
        // Key=Value ([Setup], [Messages], .isl files)
        b'=' if w > s => {
            o.put(s, w, Tok::Key);
            o.put(p, p + 1, Tok::Punct);
            let v = p + 1 + l[p + 1..].iter().take_while(|&&b| is_ws(b)).count();
            let val = l[v..].trim_ascii_end();
            if is_num_word(val) {
                o.put(v, v + val.len(), Tok::Num);
            } else if [&b"yes"[..], b"no", b"true", b"false"].iter().any(|w| val.eq_ignore_ascii_case(w)) {
                o.put(v, v + val.len(), Tok::Lit);
            } else {
                inno_value(l, v, n, o);
            }
        }
        // Name: value; Name: value
        b':' if w > s => inno_params(l, s, o),
        _ => inno_value(l, s, n, o),
    }
    st
}

/// `Name: "value"; Flags: a b; MinVersion: 6.1` from `i`.
fn inno_params(l: &[u8], mut i: usize, o: &mut Out) {
    const WORDS: [&[u8]; 6] = [b"attribs", b"flags", b"permissions", b"root", b"type", b"valuetype"];
    let n = l.len();
    loop {
        i += l[i..].iter().take_while(|&&b| is_ws(b)).count();
        let w = i + l[i..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
        let p = w + l[w..].iter().take_while(|&&b| is_ws(b)).count();
        if w == i || at(l, p) != b':' {
            inno_value(l, i, n, o);
            return;
        }
        o.put(i, w, Tok::Key);
        o.put(p, p + 1, Tok::Punct);
        // flags and the like are words from a list
        let words = WORDS.iter().any(|k| l[i..w].eq_ignore_ascii_case(k));
        let mut j = p + 1 + l[p + 1..].iter().take_while(|&&b| is_ws(b)).count();
        if at(l, j) == b'"' {
            // "quoted" ("" is a quote)
            let mut k = j + 1;
            let e = loop {
                match l[k..].iter().position(|&b| b == b'"') {
                    Some(q) if at(l, k + q + 1) == b'"' => k += q + 2,
                    Some(q) => break k + q + 1,
                    None => break n,
                }
            };
            inno_str(l, j, e, o);
            j = e;
        }
        let e = l[j..].iter().position(|&b| b == b';').map_or(n, |p| j + p);
        let v = l[j..e].trim_ascii_end();
        if words && !v.is_empty() && v.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b' ' | b'\t')) {
            o.put(j, j + v.len(), Tok::Lit);
        } else if is_num_word(v) {
            o.put(j, j + v.len(), Tok::Num);
        } else {
            inno_value(l, j, e, o);
        }
        if e >= n {
            return;
        }
        o.put(e, e + 1, Tok::Punct);
        i = e + 1;
    }
}

/// Constants (`{app}`, `{cm:Launch,{#AppName}}`, `{#Name}`; `{{` is a brace), `"strings"` and `%1` in a value.
fn inno_value(l: &[u8], mut i: usize, to: usize, o: &mut Out) {
    while i < to {
        match l[i] {
            b'{' if at(l, i + 1) == b'{' => i += 2,
            b'{' => match inno_const_end(l, i, to) {
                Some(e) => {
                    o.put(i, e, Tok::Var);
                    i = e;
                }
                None => i += 1,
            },
            b'"' => {
                let e = l[i + 1..to].iter().position(|&b| b == b'"').map_or(to, |p| i + p + 2);
                inno_str(l, i, e, o);
                i = e;
            }
            b'%' if at(l, i + 1).is_ascii_digit() || at(l, i + 1) == b'n' => {
                o.put(i, i + 2, Tok::Var);
                i += 2;
            }
            _ => i += 1,
        }
    }
}

/// A `"string"` from `s` to `e`, its constants colored.
fn inno_str(l: &[u8], s: usize, e: usize, o: &mut Out) {
    let (mut seg, mut i) = (s, s + 1);
    while i < e {
        if l[i] == b'{' && at(l, i + 1) == b'{' {
            i += 2;
            continue;
        }
        if l[i] == b'{' {
            if let Some(ce) = inno_const_end(l, i, e) {
                o.put(seg, i, Tok::Str);
                o.put(i, ce, Tok::Var);
                (seg, i) = (ce, ce);
                continue;
            }
        }
        i += 1;
    }
    o.put(seg, e, Tok::Str);
}

/// Where an Inno Setup constant at `i` (a `{`) ends, before `to`: `{app}`, `{cm:Launch,{#Name}}`.
fn inno_const_end(l: &[u8], i: usize, to: usize) -> Option<usize> {
    let mut depth = 0;
    for (k, &b) in l[i..to.min(i + 256)].iter().enumerate() {
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i + k + 1);
            }
        }
    }
    None
}

/// A preprocessor line from `i` (`#define Name "value"`, `#include "file"`, `#if Ver < 0x06000000`); `start`:
/// whether `i` is its `#` (else it goes on from the text before).
fn ispp_line(l: &[u8], mut i: usize, start: bool, o: &mut Out) {
    let n = l.len();
    // a name comes next (`#define Name`), colored as a variable
    let mut name = false;
    if start {
        let e = i + 1 + l[i + 1..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
        o.put(i, e, Tok::Control);
        const NAMED: [&[u8]; 7] = [b"define", b"undef", b"ifdef", b"ifndef", b"sub", b"dim", b"redim"];
        name = NAMED.iter().any(|w| l[i + 1..e].eq_ignore_ascii_case(w));
        i = e;
    }
    while i < n {
        let c = l[i];
        match c {
            b'"' | b'\'' => {
                let e = l[i + 1..].iter().position(|&b| b == c).map_or(n, |p| i + p + 2);
                o.put(i, e, Tok::Str);
                i = e;
            }
            b'{' if at(l, i + 1) == b'#' => {
                let e = l[i..].iter().position(|&b| b == b'}').map_or(n, |p| i + p + 1);
                o.put(i, e, Tok::Var);
                i = e;
            }
            b'/' if at(l, i + 1) == b'/' => {
                o.put(i, n, Tok::Comment);
                return;
            }
            b'/' if at(l, i + 1) == b'*' => {
                let e = find(l, i + 2, b"*/").map_or(n, |p| p + 2);
                o.put(i, e, Tok::Comment);
                i = e;
            }
            c if c.is_ascii_digit() => {
                let e = i + l[i..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
                o.put(i, e, Tok::Num);
                i = e;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let e = i + l[i..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
                if name {
                    o.put(i, e, Tok::Var);
                    name = false;
                } else if at(l, e) == b'(' {
                    o.put(i, e, Tok::Func);
                }
                i = e;
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{end_state, has, toks, view};
    use super::super::{Lang, State, Tok};

    fn line_has(line: &[(String, Tok)], s: &str, tok: Tok) -> bool {
        line.contains(&(s.to_string(), tok))
    }

    #[test]
    fn toml_documents() {
        let src = "# config\n[package]\nname = \"slate\"\nversion.major = 1_000\n\"quoted key\" = 'lit'\n[[bin]]\nwhen = 1979-05-27 07:32:00Z\nok = true\nmax = inf\n";
        let v = view(Lang::Toml, src);
        assert!(line_has(&v[0], "# config", Tok::Comment) && line_has(&v[1], "[package]", Tok::Section));
        assert!(line_has(&v[2], "name", Tok::Key) && line_has(&v[2], "\"slate\"", Tok::Str));
        assert!(line_has(&v[3], "version.major", Tok::Key) && line_has(&v[3], "1_000", Tok::Num));
        assert!(line_has(&v[4], "\"quoted key\"", Tok::Key) && line_has(&v[4], "'lit'", Tok::Str));
        assert!(line_has(&v[5], "[[bin]]", Tok::Section));
        assert!(line_has(&v[6], "1979-05-27 07:32:00Z", Tok::Num) && line_has(&v[7], "true", Tok::Lit) && line_has(&v[8], "inf", Tok::Num));
        // multi-line strings, and arrays over several lines (whose `[1, 2]` lines aren't tables)
        let src = "s = \"\"\"\nline [x] = 1 # not a comment\n\"\"\"\"\nm = [\n  [1, 2],\n  { a = 1, b = \"x\" },\n]\n[after]\n";
        let v = view(Lang::Toml, src);
        assert_eq!(v[1], vec![("line [x] = 1 # not a comment".into(), Tok::Str)]);
        assert_eq!(v[2], vec![("\"\"\"\"".into(), Tok::Str)]);
        assert!(!v[4].iter().any(|t| t.1 == Tok::Section) && line_has(&v[4], "1", Tok::Num));
        assert!(line_has(&v[5], "a", Tok::Key) && line_has(&v[5], "b", Tok::Key) && line_has(&v[5], "\"x\"", Tok::Str));
        assert!(line_has(&v[7], "[after]", Tok::Section));
        assert_eq!(end_state(Lang::Toml, src).b, 0);
        // an array left open ends at a header at the start of a line
        let v = view(Lang::Toml, "a = [1,\n[server]\nport = 80\n");
        assert!(line_has(&v[1], "[server]", Tok::Section) && line_has(&v[2], "port", Tok::Key));
    }

    #[test]
    fn nginx_configuration() {
        let src = "http {\n    server {\n        listen 80;\n        server_name example.com; # main\n        location ~* \\.php$ {\n            proxy_pass http://127.0.0.1:9000;\n            set $x \"a;b\";\n            gzip on;\n        }\n    }\n}\n";
        let v = view(Lang::Nginx, src);
        assert!(line_has(&v[0], "http", Tok::Control) && line_has(&v[1], "server", Tok::Control));
        assert!(line_has(&v[2], "listen", Tok::Keyword) && line_has(&v[2], "80", Tok::Num));
        assert!(line_has(&v[3], "server_name", Tok::Keyword) && line_has(&v[3], "# main", Tok::Comment));
        assert!(line_has(&v[4], "location", Tok::Control) && !line_has(&v[4], "\\.php", Tok::Keyword));
        assert!(line_has(&v[5], "proxy_pass", Tok::Keyword) && !v[5].iter().any(|t| t.1 == Tok::Num));
        assert!(line_has(&v[6], "$x", Tok::Var) && line_has(&v[6], "\"a;b\"", Tok::Str));
        assert!(line_has(&v[7], "on", Tok::Lit));
        // a log format over several lines: its later lines aren't directives
        let v = view(Lang::Nginx, "log_format main '$remote_addr'\n    '$status';\n");
        assert!(line_has(&v[1], "'$status'", Tok::Str));
        // `#` inside a word isn't a comment
        assert!(!toks(Lang::Nginx, "rewrite ^/a#b /c;", State::START).iter().any(|t| t.1 == Tok::Comment));
    }

    #[test]
    fn apache_configuration() {
        let src = "# main\nServerName example.com\n<VirtualHost *:80>\n    DocumentRoot \"/var/www\"\n    RewriteEngine On\n    RewriteRule ^(.*)$ https://%{HTTP_HOST}$1 [R=301,L]\n    ErrorLog ${APACHE_LOG_DIR}/error.log \\\n        # not a comment\n</VirtualHost>\n";
        let v = view(Lang::Apache, src);
        assert!(line_has(&v[0], "# main", Tok::Comment) && line_has(&v[1], "ServerName", Tok::Keyword));
        assert!(line_has(&v[2], "VirtualHost", Tok::Tag) && line_has(&v[2], "*:80", Tok::Attr) && line_has(&v[2], ">", Tok::Punct));
        assert!(line_has(&v[3], "\"/var/www\"", Tok::Str) && line_has(&v[4], "On", Tok::Lit));
        assert!(line_has(&v[5], "%{HTTP_HOST}", Tok::Var) && line_has(&v[5], "$1", Tok::Var) && line_has(&v[5], "[R=301,L]", Tok::Attr));
        assert!(line_has(&v[6], "${APACHE_LOG_DIR}", Tok::Var));
        assert!(!v[7].iter().any(|t| t.1 == Tok::Comment || t.1 == Tok::Keyword), "a continued line: {:?}", v[7]);
        assert!(line_has(&v[8], "VirtualHost", Tok::Tag));
    }

    #[test]
    fn properties_files() {
        let src = "# comment\n! also\nkey = value\nother:value\nspaced value here\nlong = one \\\n    two\nname=caf\\u00e9 ${user.home}\n";
        let v = view(Lang::Properties, src);
        assert!(line_has(&v[0], "# comment", Tok::Comment) && line_has(&v[1], "! also", Tok::Comment));
        assert!(line_has(&v[2], "key", Tok::Key) && line_has(&v[2], "=", Tok::Punct) && line_has(&v[3], "other", Tok::Key));
        assert!(line_has(&v[4], "spaced", Tok::Key) && !line_has(&v[4], "value", Tok::Key));
        assert!(v[6].iter().all(|t| t.1 != Tok::Key), "a continued value: {:?}", v[6]);
        assert!(line_has(&v[7], "\\u00e9", Tok::Lit) && line_has(&v[7], "${user.home}", Tok::Var));
        // a backslash at the end of a comment doesn't continue it
        let v = view(Lang::Properties, "# note \\\nkey=1\n");
        assert!(line_has(&v[1], "key", Tok::Key));
    }

    #[test]
    fn subtitles_srt_and_vtt() {
        let v = view(Lang::Subtitles, "1\n00:00:01,000 --> 00:00:04,000\n<i>Hello</i> {\\an8}there &amp; you\n\n");
        assert!(line_has(&v[0], "1", Tok::Num) && line_has(&v[1], "00:00:01,000", Tok::Num) && line_has(&v[1], "-->", Tok::Punct));
        assert!(line_has(&v[1], "00:00:04,000", Tok::Num) && line_has(&v[2], "<i>", Tok::Tag) && line_has(&v[2], "{\\an8}", Tok::Dim));
        let src = "WEBVTT - Title\n\nNOTE a comment\nover lines\n\nSTYLE\n::cue {\n  color: yellow;\n}\n\n00:01.000 --> 00:04.000 align:start\n<v Bob>Hi\n";
        let v = view(Lang::Subtitles, src);
        assert!(line_has(&v[0], "WEBVTT", Tok::Keyword) && line_has(&v[3], "over lines", Tok::Comment));
        assert!(line_has(&v[5], "STYLE", Tok::Keyword) && line_has(&v[7], "color", Tok::Attr));
        assert!(line_has(&v[10], "00:01.000", Tok::Num) && line_has(&v[10], "align", Tok::Attr) && line_has(&v[11], "<v Bob>", Tok::Tag));
        assert_eq!(end_state(Lang::Subtitles, src), State::START);
    }

    #[test]
    fn calendars_and_solutions() {
        has(Lang::Calendar, "DTSTART;TZID=\"Europe/Paris\":20261008T120000", &[
            ("DTSTART", Tok::Key),
            ("TZID", Tok::Attr),
            ("\"Europe/Paris\"", Tok::Str),
            ("20261008T120000", Tok::Num),
        ]);
        has(Lang::Calendar, "BEGIN:VEVENT", &[("BEGIN:VEVENT", Tok::Section)]);
        assert!(toks(Lang::Calendar, " folded:text", State::START).is_empty());
        let sln = "Microsoft Visual Studio Solution File, Format Version 12.00\n# Visual Studio Version 17\nVisualStudioVersion = 17.0.31903.59\nProject(\"{FAE04EC0-301F}\") = \"App\", \"App\\App.csproj\", \"{1234}\"\nEndProject\nGlobal\n\tGlobalSection(SolutionConfigurationPlatforms) = preSolution\n\t\t{1234}.Debug|Any CPU.ActiveCfg = Debug|Any CPU\n";
        let v = view(Lang::Sln, sln);
        assert!(line_has(&v[0], "Microsoft Visual Studio Solution File, Format Version 12.00", Tok::Heading));
        assert!(line_has(&v[1], "# Visual Studio Version 17", Tok::Comment));
        assert!(line_has(&v[2], "VisualStudioVersion", Tok::Key) && line_has(&v[2], "17.0.31903.59", Tok::Num));
        assert!(line_has(&v[3], "Project", Tok::Keyword) && line_has(&v[3], "\"{FAE04EC0-301F}\"", Tok::Num) && line_has(&v[3], "\"App\"", Tok::Str));
        assert!(line_has(&v[6], "SolutionConfigurationPlatforms", Tok::Type) && line_has(&v[6], "preSolution", Tok::Lit));
        assert!(line_has(&v[7], "{1234}", Tok::Num) && line_has(&v[7], ".Debug|Any CPU.ActiveCfg", Tok::Key));
    }

    #[test]
    fn ppcl_programs() {
        let src = "00010     C\tAIR HANDLER 1\r\n00020     DEFINE(A,\"AHU1.\")\r\n00030     LOCAL(TMR)\r\n00040     SAMPLE(5) $LOC1 = \"$TMR\" + 5\r\n00050     IF(\"%A%SAT\" .GT. 55.0 .AND. DAY .EQ. 1 .AND. TIME .EQ. 07:30) THEN ON(\"%A%CLG\") ELSE OFF(\"%A%CLG\")\r\n00060     IF(\"%A%SF\" .EQ. FAILED) THEN SET(@OPER,0,\"%A%VLV\") ELSE RELEAS(@OPER,\"%A%VLV\")\r\n00070     IF(TOTAL(\"%A%SF\") .GT. 8784) THEN GOTO 10\r\n00080     C IF(\"%A%SF\" .EQ. ON) THEN GOTO 10\r\n00090     IF(\"%A%SF\" .EQ. ON) THEN ON(\"%A%LT\")\r\n";
        let v = view(Lang::Ppcl, src);
        assert_eq!(v[0], vec![("00010".into(), Tok::Dim), ("C\tAIR HANDLER 1\r".into(), Tok::Comment)]);
        // DEFINE names an abbreviation (`%A%` later) for a text that isn't a point; LOCAL's names are declarations
        assert_eq!(v[1], vec![("00020".into(), Tok::Dim), ("DEFINE".into(), Tok::Keyword), ("A".into(), Tok::Type)]);
        assert_eq!(v[2], vec![("00030".into(), Tok::Dim), ("LOCAL".into(), Tok::Keyword)]);
        // a resident local, and one of the program's own in quotes (plain, as it's no point)
        assert!(line_has(&v[3], "SAMPLE", Tok::Keyword) && line_has(&v[3], "$LOC1", Tok::Str) && line_has(&v[3], "5", Tok::Num));
        assert!(!v[3].iter().any(|t| t.0.contains("$TMR")), "{:?}", v[3]);
        // points (and the abbreviations in them), numbers and times; resident points as values; operators plain
        assert!(line_has(&v[4], "IF", Tok::Keyword) && line_has(&v[4], "%A%", Tok::Type) && line_has(&v[4], "SAT\"", Tok::Var));
        assert!(line_has(&v[4], "55.0", Tok::Num) && line_has(&v[4], "07:30", Tok::Num));
        assert!(line_has(&v[4], "DAY", Tok::Str) && line_has(&v[4], "TIME", Tok::Str));
        assert!(line_has(&v[4], "THEN", Tok::Keyword) && line_has(&v[4], "ON", Tok::Keyword) && line_has(&v[4], "ELSE", Tok::Keyword));
        assert!(!v[4].iter().any(|t| t.0.contains('.') && t.1 != Tok::Var && t.1 != Tok::Num), "{:?}", v[4]);
        assert!(line_has(&v[5], "FAILED", Tok::Str) && line_has(&v[5], "@OPER", Tok::Str) && line_has(&v[5], "RELEAS", Tok::Keyword));
        assert!(line_has(&v[6], "TOTAL", Tok::Keyword) && line_has(&v[6], "GOTO", Tok::Keyword) && line_has(&v[6], "10", Tok::Num));
        // a statement commented out is all comment
        assert_eq!(v[7].len(), 2);
        assert_eq!(v[7][1].1, Tok::Comment);
        // `ON` compared with is a value, `ON(…)` a command
        let ons: Vec<Tok> = v[8].iter().filter(|t| t.0 == "ON").map(|t| t.1).collect();
        assert_eq!(ons, [Tok::Str, Tok::Keyword]);
        assert!(!v.iter().flatten().any(|t| t.1 == Tok::Error), "{v:?}");
        let end = end_state(Lang::Ppcl, src);
        assert_eq!((end.kind, end.b), (0, 90));
        // without line numbers, in lower case
        has(Lang::Ppcl, "c a comment", &[("c a comment", Tok::Comment)]);
        has(Lang::Ppcl, "if(\"x\" .eq. on) then goto 20", &[("if", Tok::Keyword), ("on", Tok::Str), ("goto", Tok::Keyword)]);
        // `C` is a word, not a column: `CLG = 1` is an assignment
        has(Lang::Ppcl, "00100     CLG = 1", &[("CLG", Tok::Var), ("1", Tok::Num)]);
        // a number stops at an operator without spaces
        has(Lang::Ppcl, "IF($LOC1 .GT.5.AND.NODE5 .EQ. FAILED .OR. SECND3 .GT. 60)", &[
            ("5", Tok::Num),
            ("NODE5", Tok::Str),
            ("SECND3", Tok::Str),
        ]);
    }

    #[test]
    fn ppcl_as_other_exports_write_it() {
        // short numbers and tabs, a line turned off, a statement the compiler couldn't read, an OIP keystroke string
        let src = "10\tC AHU-1 SUPPLY FAN\n20\tIF(TIME .GT. 6.00 .AND. TIME .LT. 18.00) THEN ON(\"SFAN\") ELSE OFF(\"SFAN\")\n# 30\tSET(1,\"X\")\n40\tUNKNOWN (SET(1,\"Y\"))\n50\tOIP(TRIG,\"P/T/D/H///SITE.TOWER.AHU01.SFAN/1/\")\n60\tONERR(70)\n70\tGOTO 10\n";
        let v = view(Lang::Ppcl, src);
        assert!(line_has(&v[0], "10", Tok::Dim) && line_has(&v[0], "C AHU-1 SUPPLY FAN", Tok::Comment));
        assert!(line_has(&v[1], "6.00", Tok::Num) && line_has(&v[1], "\"SFAN\"", Tok::Var));
        assert_eq!(v[2], vec![("#".into(), Tok::Dim), ("30".into(), Tok::Dim), ("\tSET(1,\"X\")".into(), Tok::Dim)]);
        assert!(line_has(&v[3], "UNKNOWN", Tok::Error) && line_has(&v[3], " (SET(1,\"Y\"))", Tok::Dim));
        assert!(line_has(&v[4], "OIP", Tok::Keyword) && line_has(&v[4], "TRIG", Tok::Var));
        assert!(!v[4].iter().any(|t| t.0.contains("SFAN")), "keystrokes aren't a point: {:?}", v[4]);
        assert!(line_has(&v[5], "ONERR", Tok::Keyword));
        assert!(!v.iter().flatten().any(|t| t.1 == Tok::Error && t.0 != "UNKNOWN"), "{v:?}");
        // the dot: only the eleven operators split a word
        has(Lang::Ppcl, "\"ROOM.MIN.TEMP\" = A.ROOT.B", &[("\"ROOM.MIN.TEMP\"", Tok::Var), ("A", Tok::Var), ("B", Tok::Var)]);
        has(Lang::Ppcl, "00010     X = ROOM.MIN.TEMP", &[("ROOM.MIN.TEMP", Tok::Error)]);
        // a command PPCL doesn't have stays plain
        assert!(toks(Lang::Ppcl, "00010     FOO(1)", State::START).iter().all(|t| t.0 != "FOO"));
    }

    #[test]
    fn ppcl_mistakes_in_red() {
        let err = |text: &str, want: &str| has(Lang::Ppcl, text, &[(want, Tok::Error)]);
        err("00010     SET(1,\"OPEN", "\"OPEN");
        err("00010     GOTO END", "GOTO");
        err("00010     IF(X .EQ. 1) THEN GOSUB", "GOSUB");
        err("00010     GOTO 40000", "GOTO");
        err("00010     IF(X .NOT. 1) THEN ON(\"A\")", ".NOT.");
        err("00010     IF(X .EQ 1) THEN ON(\"A\")", ".EQ");
        err("00010     SET(@HIGH,1,\"A\")", "@HIGH");
        err("00010     ON(SUPPLYFAN)", "SUPPLYFAN");
        // line numbers out of order, the same twice, or too big; the next one is compared with the one before it
        let v = view(Lang::Ppcl, "00020     C\n00010     C\n00010     C\n00030     C\n40000     C\n00040     C\n");
        let nums: Vec<Tok> = v.iter().map(|l| l[0].1).collect();
        assert_eq!(nums, [Tok::Dim, Tok::Error, Tok::Error, Tok::Dim, Tok::Error, Tok::Dim]);
        // a heading between programs, or a blank line, starts the count over; a heading gets no red
        let v = view(Lang::Ppcl, "00020     C\nPROGRAM 2 Panel: PXCPANEL0001 \"open\n00010     C\n00020     C\n\r\n00010     C\n");
        assert_eq!(v[2][0], ("00010".into(), Tok::Dim));
        assert_eq!(v[5][0], ("00010".into(), Tok::Dim));
        assert!(!v[1].iter().any(|t| t.1 == Tok::Error), "{:?}", v[1]);
        // (a long name in quotes is quick)
        let long = format!("00010     ON(\"{}%X%\")", "A".repeat(200_000));
        assert!(toks(Lang::Ppcl, &long, State::START).iter().any(|t| t.1 == Tok::Type));
    }

    #[test]
    fn ppcl_word_lists_are_sorted() {
        use super::{PPCL_COMMANDS, PPCL_OPERATORS, PPCL_PRIORITIES, PPCL_VALUES, PPCL_WORDS};
        for list in [&PPCL_COMMANDS[..], &PPCL_WORDS, &PPCL_VALUES, &PPCL_PRIORITIES, &PPCL_OPERATORS] {
            for w in list.windows(2) {
                assert!(w[0] < w[1], "{:?} before {:?}", String::from_utf8_lossy(w[0]), String::from_utf8_lossy(w[1]));
            }
            assert!(list.iter().all(|w| w.len() <= 12 && !w.iter().any(u8::is_ascii_lowercase)));
        }
    }

    #[test]
    fn gcode_programs() {
        let src = ";FLAVOR:Marlin\nM104 S200 ; heat\nG28 ; home\nN10 G1 X10.5 Y-2 E.5 F1200*45\nM117 Printing layer 1\nT1\nG1 X#1 (note) Z[#2*2]\nSET_FAN_SPEED FAN=part SPEED=0.5\n%\nO1001\no100 sub\ng1x5y6\n";
        let v = view(Lang::GCode, src);
        assert_eq!(v[0], vec![(";FLAVOR:Marlin".into(), Tok::Comment)]);
        assert!(line_has(&v[1], "M104", Tok::Control) && line_has(&v[1], "S", Tok::Attr) && line_has(&v[1], "200", Tok::Num));
        assert!(line_has(&v[1], "; heat", Tok::Comment) && line_has(&v[2], "G28", Tok::Keyword));
        assert!(line_has(&v[3], "N10", Tok::Dim) && line_has(&v[3], "G1", Tok::Keyword) && line_has(&v[3], "10.5", Tok::Num));
        assert!(line_has(&v[3], "-2", Tok::Num) && line_has(&v[3], ".5", Tok::Num) && line_has(&v[3], "*45", Tok::Dim));
        assert!(line_has(&v[4], "M117", Tok::Control) && line_has(&v[4], "Printing layer 1", Tok::Str) && line_has(&v[5], "T1", Tok::Type));
        assert!(line_has(&v[6], "X", Tok::Attr) && line_has(&v[6], "#1", Tok::Var) && line_has(&v[6], "(note)", Tok::Comment));
        assert!(line_has(&v[6], "#2", Tok::Var) && line_has(&v[6], "2", Tok::Num));
        assert!(line_has(&v[7], "SET_FAN_SPEED", Tok::Func) && line_has(&v[7], "SPEED", Tok::Attr) && line_has(&v[7], "0.5", Tok::Num));
        assert!(line_has(&v[8], "%", Tok::Section) && line_has(&v[9], "O1001", Tok::Section));
        assert!(line_has(&v[10], "o100", Tok::Section) && line_has(&v[10], "sub", Tok::Keyword));
        assert!(line_has(&v[11], "g1", Tok::Keyword) && line_has(&v[11], "x", Tok::Attr) && line_has(&v[11], "6", Tok::Num));
        // Fanuc's `;` at a line's end ends the block: it's no (empty) comment
        let v = view(Lang::GCode, "O1001 (BRACKET; OP1);\nN10 G90 G21 ;\r\n");
        assert!(line_has(&v[0], "(BRACKET; OP1)", Tok::Comment) && line_has(&v[0], ";", Tok::Punct));
        assert!(line_has(&v[1], ";\r", Tok::Punct) && !v[1].iter().any(|t| t.1 == Tok::Comment));
    }

    #[test]
    fn inno_setup_scripts() {
        let src = "; Script\n#define MyAppName \"Demo\"\n[Setup]\nAppName={#MyAppName}\nDefaultDirName={autopf}\\Demo\nSolidCompression=yes\n\n[Files]\nSource: \"bin\\*\"; DestDir: \"{app}\"; Flags: ignoreversion recursesubdirs\n[Registry]\nRoot: HKLM; Subkey: \"Software\\Demo\"; ValueType: string; ValueData: \"{{x}\"\n[Code]\n{ a comment\n  [still] }\nfunction InitializeSetup(): Boolean;\nvar S: String;\nbegin\n  S := 'it''s' + #13#10 + IntToStr($FF); (* note *)\n  Result := MsgBox('{#MyAppName}', mbInformation, MB_OK) = IDOK; // ok\n  if CurPageID in\n    [wpReady] then Exit;\nend;\n[Run]\nFilename: \"{app}\\demo.exe\"; Flags: nowait postinstall\n";
        let v = view(Lang::InnoSetup, src);
        assert_eq!(v[0], vec![("; Script".into(), Tok::Comment)]);
        assert!(line_has(&v[1], "#define", Tok::Control) && line_has(&v[1], "MyAppName", Tok::Var) && line_has(&v[1], "\"Demo\"", Tok::Str));
        assert!(line_has(&v[2], "[Setup]", Tok::Section) && line_has(&v[3], "AppName", Tok::Key) && line_has(&v[3], "{#MyAppName}", Tok::Var));
        assert!(line_has(&v[4], "{autopf}", Tok::Var) && line_has(&v[5], "yes", Tok::Lit));
        assert!(line_has(&v[8], "Source", Tok::Key) && line_has(&v[8], "\"bin\\*\"", Tok::Str) && line_has(&v[8], "DestDir", Tok::Key));
        assert!(line_has(&v[8], "{app}", Tok::Var) && line_has(&v[8], "ignoreversion recursesubdirs", Tok::Lit));
        assert!(line_has(&v[10], "HKLM", Tok::Lit) && line_has(&v[10], "string", Tok::Lit) && line_has(&v[10], "\"{{x}\"", Tok::Str));
        assert!(line_has(&v[11], "[Code]", Tok::Section) && line_has(&v[12], "{ a comment", Tok::Comment) && line_has(&v[13], "  [still] }", Tok::Comment));
        assert!(line_has(&v[14], "function", Tok::Keyword) && line_has(&v[14], "InitializeSetup", Tok::Func) && line_has(&v[14], "Boolean", Tok::Type));
        assert!(line_has(&v[15], "var", Tok::Keyword) && line_has(&v[15], "String", Tok::Type) && line_has(&v[16], "begin", Tok::Control));
        assert!(line_has(&v[17], "'it'", Tok::Str) && line_has(&v[17], "'s'", Tok::Str) && line_has(&v[17], "#13", Tok::Str));
        assert!(line_has(&v[17], "$FF", Tok::Num) && line_has(&v[17], "IntToStr", Tok::Func) && line_has(&v[17], "(* note *)", Tok::Comment));
        assert!(line_has(&v[18], "Result", Tok::Var) && line_has(&v[18], "MsgBox", Tok::Func) && line_has(&v[18], "// ok", Tok::Comment));
        // a line that starts with a set isn't a section
        assert!(line_has(&v[20], "then", Tok::Control) && !line_has(&v[20], "[wpReady]", Tok::Section));
        assert!(line_has(&v[22], "[Run]", Tok::Section) && line_has(&v[23], "Filename", Tok::Key) && line_has(&v[23], "nowait postinstall", Tok::Lit));
        assert_eq!(end_state(Lang::InnoSetup, src), State::START);
        // the preprocessor in [Code], and a comment left open there by a section's start
        let v = view(Lang::InnoSetup, "[Code]\n#ifdef X\n{ open\n[Files]\nSource: x\n");
        assert!(line_has(&v[1], "#ifdef", Tok::Control) && line_has(&v[1], "X", Tok::Var));
        assert!(line_has(&v[3], "[Files]", Tok::Section) && line_has(&v[4], "Source", Tok::Key));
    }
}
