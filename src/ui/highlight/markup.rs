//! Markup and data languages: XML and HTML (with the scripts and styles inside HTML pages), Markdown and YAML.

use super::code;
use super::{Lang, Out, Span, State, Tok, at, find, is_num_word, line_end, scan_str};

// ---- XML and HTML ----

// What `State::kind` means here.
const TEXT: u8 = 0;
/// Inside `<name …>` (attributes); `b`: what the element's content is (`SCRIPT`, `STYLE`, or 0).
const TAG: u8 = 1;
/// A quoted attribute value; `a`: the quote, `b` as for `TAG`.
const VALUE: u8 = 2;
const COMMENT: u8 = 3;
const CDATA: u8 = 4;
/// `<?xml …?>`
const PI: u8 = 5;
/// `<!DOCTYPE …>`; `a`: depth of `[…]`, `b`: in a quoted string (its quote), or in the subset a comment (1) or a
/// processing instruction (2).
const DOCTYPE: u8 = 6;

/// `State::mode` inside an HTML `<script>` / `<style>` element.
const SCRIPT: u8 = 1;
const STYLE: u8 = 2;

fn name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':' || c >= 0x80
}

fn name_end(t: &[u8], mut i: usize) -> usize {
    while i < t.len() && (t[i].is_ascii_alphanumeric() || matches!(t[i], b'_' | b':' | b'-' | b'.') || t[i] >= 0x80) {
        i += 1;
    }
    i
}

/// `&amp;`, `&#123;`, `&#x1F;` at `i`: its end.
fn entity_end(t: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if at(t, j) == b'#' {
        j += 1;
    }
    let s = j;
    while j < t.len() && j - s < 32 && t[j].is_ascii_alphanumeric() {
        j += 1;
    }
    (j > s && at(t, j) == b';').then_some(j + 1)
}

/// The next `</script` (or `</style`) at or after `from`, in any letter case.
fn find_end_tag(t: &[u8], mut from: usize, tag: &[u8]) -> Option<usize> {
    while let Some(p) = memchr::memchr(b'<', &t[from.min(t.len())..]) {
        let i = from + p;
        if t.len() >= i + tag.len() && t[i..i + tag.len()].eq_ignore_ascii_case(tag) {
            return Some(i);
        }
        from = i + 1;
    }
    None
}

/// Colors from `s` up to and including `end` (found from `from`); when the text ends first, the state to go on in.
fn until(t: &[u8], s: usize, from: usize, end: &[u8], tok: Tok, kind: u8, st: State, o: &mut Out) -> Result<usize, State> {
    match find(t, from, end) {
        Some(p) => {
            o.put(s, p + end.len(), tok);
            Ok(p + end.len())
        }
        None => {
            o.put(s, t.len(), tok);
            Err(State { kind, a: 0, ..st })
        }
    }
}

/// A `<!DOCTYPE …>` from `s`, scanning from `i` with `depth` open `[` and in `sub` what hides its brackets and its
/// `>`: a quoted string (its quote), or in the `[…]` subset a comment (1) or a processing instruction (2), which
/// are colored as such (`<!-- the catalog's [elements] -->`).
#[allow(clippy::too_many_arguments)]
fn doctype(t: &[u8], mut s: usize, mut i: usize, mut depth: u8, mut sub: u16, st: State, o: &mut Out) -> Result<usize, State> {
    let n = t.len();
    while i < n {
        if sub == 1 || sub == 2 {
            let (end, tok): (&[u8], Tok) = if sub == 1 { (b"-->", Tok::Comment) } else { (b"?>", Tok::Section) };
            let Some(p) = find(t, i, end) else {
                o.put(s, n, tok);
                return Err(State { kind: DOCTYPE, a: depth, b: sub, ..st });
            };
            o.put(s, p + end.len(), tok);
            (i, s, sub) = (p + end.len(), p + end.len(), 0);
            continue;
        }
        if sub != 0 {
            // in a quoted string
            let Some(p) = memchr::memchr(sub as u8, &t[i..]) else { break };
            (i, sub) = (i + p + 1, 0);
            continue;
        }
        match t[i] {
            b'"' | b'\'' => sub = t[i] as u16,
            b'<' if depth > 0 && (t[i..].starts_with(b"<!--") || t[i..].starts_with(b"<?")) => {
                o.put(s, i, Tok::Keyword);
                let comment = t[i + 1] == b'!';
                (s, sub) = (i, if comment { 1 } else { 2 });
                i += if comment { 4 } else { 2 };
                continue;
            }
            b'[' => depth = depth.saturating_add(1),
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => {
                o.put(s, i + 1, Tok::Keyword);
                return Ok(i + 1);
            }
            _ => {}
        }
        i += 1;
    }
    o.put(s, n, Tok::Keyword);
    Err(State { kind: DOCTYPE, a: depth, b: sub, ..st })
}

/// A quoted attribute value from `s`, closing quote `q` searched from `from`.
fn value(t: &[u8], s: usize, from: usize, q: u8, st: State, o: &mut Out) -> Result<usize, State> {
    match memchr::memchr(q, &t[from.min(t.len())..]) {
        Some(p) => {
            o.put(s, from + p + 1, Tok::Str);
            Ok(from + p + 1)
        }
        None => {
            o.put(s, t.len(), Tok::Str);
            Err(State { kind: VALUE, a: q, ..st })
        }
    }
}

pub(super) fn markup(html: bool, t: &[u8], mut st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut i = 0;
    macro_rules! step {
        ($e:expr, $then:expr) => {
            match $e {
                Ok(e) => {
                    i = e;
                    st.kind = $then;
                    st.a = 0;
                }
                Err(s) => return s,
            }
        };
    }
    loop {
        if st.mode != 0 {
            // A script or style: that language, up to its end tag.
            let tag: &[u8] = if st.mode == SCRIPT { b"</script" } else { b"</style" };
            let e = find_end_tag(t, i, tag).unwrap_or(n);
            let inner = State { mode: 0, ..st };
            let mut sub = o.at(i);
            let s = if st.mode == SCRIPT {
                code::code(code::syntax(Lang::JavaScript), &t[i..e], inner, &mut sub)
            } else {
                code::css(&t[i..e], inner, &mut sub)
            };
            if e == n {
                return State { mode: st.mode, ..s };
            }
            st = State { kind: TEXT, a: 0, b: 0, mode: 0, ..s };
            i = e;
        }
        if i >= n {
            return st;
        }
        match st.kind {
            TAG => {
                // attributes, up to '>' or '/>'
                while i < n {
                    let c = t[i];
                    if c == b'>' {
                        o.put(i, i + 1, Tok::Punct);
                        i += 1;
                        st = State { kind: TEXT, a: 0, b: 0, mode: st.b as u8, ..st };
                        break;
                    }
                    if c == b'/' && at(t, i + 1) == b'>' {
                        o.put(i, i + 2, Tok::Punct);
                        i += 2;
                        st = State { kind: TEXT, a: 0, b: 0, ..st };
                        break;
                    }
                    if c == b'"' || c == b'\'' {
                        match value(t, i, i + 1, c, st, o) {
                            Ok(e) => i = e,
                            Err(s) => return s,
                        }
                        continue;
                    }
                    if c == b'=' {
                        o.put(i, i + 1, Tok::Punct);
                        i += 1;
                        // HTML allows values without quotes (which can't hold quotes, `=`, `<`, `>` or backticks)
                        let v = i + t[i..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
                        if html && v < n && !matches!(t[v], b'"' | b'\'' | b'>' | b'\n' | b'\r') {
                            let e = v + t[v..]
                                .iter()
                                .take_while(|&&b| !b.is_ascii_whitespace() && !matches!(b, b'>' | b'"' | b'\'' | b'=' | b'<' | b'`'))
                                .count();
                            o.put(v, e, Tok::Str);
                            i = e;
                        }
                        continue;
                    }
                    if name_start(c) {
                        let e = name_end(t, i);
                        o.put(i, e, Tok::Attr);
                        i = e;
                        continue;
                    }
                    i += 1;
                }
            }
            VALUE => step!(value(t, i, i, st.a, st, o), TAG),
            COMMENT => step!(until(t, i, i, b"-->", Tok::Comment, COMMENT, st, o), TEXT),
            CDATA => step!(until(t, i, i, b"]]>", Tok::Str, CDATA, st, o), TEXT),
            PI => step!(until(t, i, i, b"?>", Tok::Section, PI, st, o), TEXT),
            DOCTYPE => match doctype(t, i, i, st.a, st.b, st, o) {
                Ok(e) => {
                    i = e;
                    st = State { kind: TEXT, a: 0, b: 0, ..st };
                }
                Err(s) => return s,
            },
            _ => {
                // text: find the next tag or entity
                let Some(p) = memchr::memchr2(b'<', b'&', &t[i..]) else { return st };
                let j = i + p;
                if t[j] == b'&' {
                    match entity_end(t, j) {
                        Some(e) => {
                            o.put(j, e, Tok::Lit);
                            i = e;
                        }
                        None => i = j + 1,
                    }
                    continue;
                }
                let rest = &t[j..];
                if rest.starts_with(b"<!--") {
                    step!(until(t, j, j + 4, b"-->", Tok::Comment, COMMENT, st, o), TEXT);
                    continue;
                }
                if rest.starts_with(b"<![CDATA[") {
                    step!(until(t, j, j + 9, b"]]>", Tok::Str, CDATA, st, o), TEXT);
                    continue;
                }
                if rest.starts_with(b"<?") {
                    step!(until(t, j, j + 2, b"?>", Tok::Section, PI, st, o), TEXT);
                    continue;
                }
                if rest.starts_with(b"<!") {
                    step!(doctype(t, j, j + 2, 0, 0, st, o), TEXT);
                    continue;
                }
                let close = at(t, j + 1) == b'/';
                let ns = j + 1 + close as usize;
                if !name_start(at(t, ns)) {
                    i = j + 1;
                    continue;
                }
                let ne = name_end(t, ns);
                o.put(j, ns, Tok::Punct);
                o.put(ns, ne, Tok::Tag);
                let name = &t[ns..ne];
                let content = if html && !close && name.eq_ignore_ascii_case(b"script") {
                    SCRIPT
                } else if html && !close && name.eq_ignore_ascii_case(b"style") {
                    STYLE
                } else {
                    0
                };
                st = State { kind: TAG, a: 0, b: content as u16, ..st };
                i = ne;
            }
        }
    }
}

// ---- PHP pages ----

/// `State::mode` inside PHP code: this bit, plus what the HTML around it was in, to go back to after `?>`: script
/// or style (bits 0-1), text, a tag, an attribute value or a comment (bits 2-3), the value's quote (bits 4-5).
const IN_PHP: u8 = 0x80;

/// A PHP file: an HTML page with PHP code between `<?php` (or `<?=`, `<?`) and `?>`. (Lexing all of it as PHP
/// colored an apostrophe in the page's text as the start of a string running to the end of the file.)
pub(super) fn php(t: &[u8], mut st: State, o: &mut Out) -> State {
    let mut i = 0;
    loop {
        if st.mode & IN_PHP != 0 {
            let saved = st.mode;
            let (s, close) = code::php_code(&t[i..], State { mode: 0, ..st }, &mut o.at(i));
            let Some(e) = close else { return State { mode: saved, ..s } };
            i += e;
            let quote = match (saved >> 4) & 3 {
                1 => b'"',
                2 => b'\'',
                _ => 0,
            };
            let kind = match (saved >> 2) & 3 {
                1 => TAG,
                2 if quote != 0 => VALUE,
                3 => COMMENT,
                _ => TEXT,
            };
            st = State { kind, a: quote, b: 0, mode: saved & 3, ..s };
        }
        // the page, up to the next `<?`
        let p = memchr::memmem::find(&t[i..], b"<?").map(|p| i + p);
        let s = markup(true, &t[i..p.unwrap_or(t.len())], st, &mut o.at(i));
        let Some(p) = p else { return s };
        let len = if t.len() >= p + 5 && t[p + 2..p + 5].eq_ignore_ascii_case(b"php") {
            5
        } else if at(t, p + 2) == b'=' {
            3
        } else {
            2
        };
        o.put(p, p + len, Tok::Control);
        i = p + len;
        // (in a script or style only that is kept: its own state starts again after `?>`)
        let (kind, quote) = match (s.mode, s.kind) {
            (0, TAG) => (1, 0),
            (0, VALUE) => (2, if s.a == b'"' { 1 } else { 2 }),
            (0, COMMENT) => (3, 0),
            _ => (0, 0),
        };
        st = State { kind: 0, a: 0, b: 0, mode: IN_PHP | (s.mode & 3) | kind << 2 | quote << 4, ..s };
    }
}

// ---- Markdown ----

/// Inside a fenced code block; `a`: the fence character, `b`: the fence length; `mode`: 0, or 1 + the index (in
/// `Lang::ALL`) of the language its lines are colored as, each on its own.
const FENCE: u8 = 1;
const HTML_COMMENT: u8 = 2;
/// In `mode`: inside a ``` block (three backticks, or `~~~` with `FENCE_TILDE`) colored as the language whose index
/// is in the low 6 bits, whose lexer's state `kind`, `a` and `b` are.
const FENCE_LANG: u8 = 0x80;
const FENCE_TILDE: u8 = 0x40;

const _: () = assert!(Lang::ALL.len() < 63);

fn indent(l: &[u8]) -> usize {
    l.iter().take_while(|&&b| b == b' ' || b == b'\t').count()
}

/// The language a code fence's info string names (```` ```rust ````, ```` ```{r} ````, ```` ```js title="x" ````).
fn fence_lang(info: &[u8]) -> Option<Lang> {
    let s = info.trim_ascii_start();
    let s = s.strip_prefix(b"{").unwrap_or(s);
    let s = s.strip_prefix(b".").unwrap_or(s);
    let w = s.iter().take(24).take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'#' | b'-' | b'_' | b'.')).count();
    let name = s[..w].to_ascii_lowercase();
    use Lang::*;
    Some(match name.as_slice() {
        b"rust" | b"rs" => Rust,
        b"python" | b"py" | b"python3" | b"py3" => Python,
        b"javascript" | b"js" | b"jsx" | b"mjs" | b"cjs" | b"node" => JavaScript,
        b"typescript" | b"ts" | b"tsx" => TypeScript,
        b"json" | b"jsonc" | b"json5" | b"jsonl" | b"geojson" => Json,
        b"shell" | b"sh" | b"bash" | b"zsh" | b"ksh" | b"fish" | b"console" | b"shellsession" | b"make" | b"makefile" => Shell,
        b"powershell" | b"ps" | b"ps1" | b"pwsh" | b"posh" => PowerShell,
        b"batch" | b"bat" | b"cmd" | b"dos" | b"batchfile" => Batch,
        b"c" | b"h" => C,
        b"cpp" | b"c++" | b"cc" | b"cxx" | b"hpp" | b"arduino" | b"ino" => Cpp,
        b"csharp" | b"cs" | b"c#" => CSharp,
        b"java" | b"groovy" | b"gradle" | b"jenkinsfile" => Java,
        b"kotlin" | b"kt" | b"kts" => Kotlin,
        b"swift" => Swift,
        b"go" | b"golang" => Go,
        b"php" => Php,
        b"ruby" | b"rb" => Ruby,
        b"lua" => Lua,
        b"sql" | b"mysql" | b"postgresql" | b"postgres" | b"psql" | b"plsql" | b"tsql" | b"sqlite" => Sql,
        b"yaml" | b"yml" => Yaml,
        b"xml" | b"xsd" | b"xsl" | b"xslt" | b"svg" | b"xaml" | b"plist" | b"csproj" => Xml,
        b"html" | b"htm" | b"xhtml" | b"vue" | b"svelte" => Html,
        b"css" | b"scss" | b"sass" | b"less" => Css,
        b"ini" | b"cfg" | b"conf" | b"editorconfig" | b"gitconfig" | b"dotenv" | b"env" => Ini,
        b"toml" => Toml,
        b"diff" | b"patch" | b"udiff" => Diff,
        b"dockerfile" | b"docker" | b"containerfile" => Dockerfile,
        b"csv" => Csv,
        b"tsv" => Tsv,
        b"log" => Log,
        b"vb" | b"vba" | b"vbs" | b"vbscript" | b"vbnet" | b"vb.net" => Vb,
        b"ahk" | b"autohotkey" => AutoHotkey,
        b"nginx" | b"nginxconf" => Nginx,
        b"apache" | b"apacheconf" | b"htaccess" => Apache,
        b"perl" | b"pl" | b"pm" => Perl,
        b"r" => R,
        b"hcl" | b"terraform" | b"tf" | b"tfvars" => Hcl,
        b"cmake" => CMake,
        b"properties" | b"jproperties" => Properties,
        b"srt" | b"vtt" | b"webvtt" => Subtitles,
        b"ics" | b"ical" | b"icalendar" | b"vcard" | b"vcf" => Calendar,
        b"sln" => Sln,
        b"ppcl" | b"pcl" => Ppcl,
        b"dart" => Dart,
        b"scala" | b"sc" | b"sbt" => Scala,
        b"objc" | b"objective-c" | b"objectivec" | b"obj-c" => ObjC,
        b"objc++" | b"objective-c++" | b"objectivec++" | b"obj-c++" | b"objcpp" | b"mm" => ObjCpp,
        b"gcode" | b"g-code" | b"gco" | b"nc" | b"ngc" => GCode,
        b"iss" | b"isl" | b"inno" | b"innosetup" | b"inno-setup" => InnoSetup,
        b"nsis" | b"nsi" | b"nsh" => Nsis,
        b"md" | b"markdown" => Markdown,
        _ => return None,
    })
}

/// A language's place in `Lang::ALL` (fits in 6 bits), and back.
fn lang_index(lang: Lang) -> u8 {
    Lang::ALL.iter().position(|&l| l == lang).unwrap_or(0) as u8
}

fn lang_at(i: u8) -> Lang {
    Lang::ALL.get(i as usize).copied().unwrap_or(Lang::Plain)
}

/// Colors fenced code as `lang` (PHP as PHP code: a snippet seldom starts with `<?php`).
fn fence_lex(lang: Lang, text: &[u8], st: State, o: &mut Out) -> State {
    match lang {
        Lang::Php => code::code(code::syntax(Lang::Php), text, st, o),
        _ => super::lex_in(lang, text, st, o),
    }
}

/// Whether `l` (at a line start) closes a fence of `len` or more `c`.
fn fence_closes(l: &[u8], c: u8, len: usize) -> bool {
    let ind = indent(l);
    let run = l[ind..].iter().take_while(|&&b| b == c).count();
    ind <= 3 && run >= len && l[ind + run..].trim_ascii().is_empty()
}

pub(super) fn markdown(t: &[u8], mut st: State, o: &mut Out) -> State {
    let mut start = 0;
    let (mut col0, mut bol) = (st.col0, st.bol);
    loop {
        let end = line_end(t, start);
        let nl = end < t.len();
        st = md_line(&t[start..end + nl as usize], nl, col0, bol, st, &mut o.at(start));
        if !nl {
            return st;
        }
        start = end + 1;
        (col0, bol) = (true, true);
    }
}

/// One line of Markdown (`full`: with its line break when `nl`).
fn md_line(full: &[u8], nl: bool, col0: bool, bol: bool, mut st: State, o: &mut Out) -> State {
    let l = &full[..full.len() - nl as usize];
    let n = l.len();
    if st.mode & FENCE_LANG != 0 {
        // a ``` block colored as its language, with that lexer's state kept from line to line
        let fc = if st.mode & FENCE_TILDE != 0 { b'~' } else { b'`' };
        if col0 && fence_closes(l, fc, 3) {
            o.put(0, n, Tok::Punct);
            return State { kind: 0, a: 0, b: 0, mode: 0, ..st };
        }
        let idx = st.mode & 0x3F;
        let s = fence_lex(lang_at(idx), full, State { mode: 0, col0, bol, ..st }, o);
        if s.mode != 0 {
            // its state doesn't fit: the rest of the block is colored line by line
            return State { kind: FENCE, a: fc, b: 3, mode: idx + 1, ..st };
        }
        return State { kind: s.kind, a: s.a, b: s.b, ..st };
    }
    if st.kind == FENCE {
        if col0 && fence_closes(l, st.a, st.b as usize) {
            o.put(0, n, Tok::Punct);
            return State { kind: 0, a: 0, b: 0, mode: 0, ..st };
        }
        if st.mode == 0 {
            o.put(0, n, Tok::Str);
        } else if o.on() {
            // each line from its language's line start
            fence_lex(lang_at(st.mode - 1), l, State::START, o);
        }
        return st;
    }
    let mut i = 0;
    if st.kind == HTML_COMMENT {
        match find(l, 0, b"-->") {
            Some(p) => {
                o.put(0, p + 3, Tok::Comment);
                i = p + 3;
                st.kind = 0;
            }
            None => {
                o.put(0, n, Tok::Comment);
                return st;
            }
        }
    }
    if col0 && i == 0 {
        let ind = indent(l);
        let rest = &l[ind..];
        let c = at(rest, 0);
        if ind <= 3 {
            // ``` or ~~~ code fence, with the language its code is in
            let run = rest.iter().take_while(|&&b| b == c).count();
            if (c == b'`' || c == b'~') && run >= 3 && !(c == b'`' && rest[run..].contains(&b'`')) {
                o.put(0, n, Tok::Punct);
                return match fence_lang(&rest[run..]) {
                    Some(lang) if run == 3 && lang != Lang::Markdown => {
                        let tilde = if c == b'~' { FENCE_TILDE } else { 0 };
                        State { kind: 0, a: 0, b: 0, mode: FENCE_LANG | tilde | lang_index(lang), ..st }
                    }
                    Some(lang) => State { kind: FENCE, a: c, b: run as u16, mode: lang_index(lang) + 1, ..st },
                    None => State { kind: FENCE, a: c, b: run as u16, mode: 0, ..st },
                };
            }
            // # heading
            if (1..=6).contains(&run) && c == b'#' && matches!(at(rest, run), 0 | b' ' | b'\t' | b'\r') {
                o.put(ind, n, Tok::Heading);
                return st;
            }
            // > quote
            if c == b'>' {
                o.put(ind, n, Tok::Comment);
                return st;
            }
            // --- *** ___ rules, and table header lines like |---|:--:|
            let only = |set: &[u8]| rest.iter().all(|b| set.contains(b) || *b == b' ' || *b == b'\t' || *b == b'\r');
            if (matches!(c, b'-' | b'*' | b'_') && rest.iter().filter(|&&b| b == c).count() >= 3 && only(&[c]))
                || (rest.contains(&b'|') && rest.contains(&b'-') && only(b"|-:"))
            {
                o.put(ind, n, Tok::Punct);
                return st;
            }
        }
        // list markers: - item, * item, 1. item
        let m = if matches!(c, b'-' | b'*' | b'+') && matches!(at(rest, 1), b' ' | b'\t') {
            1
        } else {
            let d = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            if (1..=9).contains(&d) && matches!(at(rest, d), b'.' | b')') && matches!(at(rest, d + 1), b' ' | b'\t') { d + 1 } else { 0 }
        };
        if m > 0 {
            o.put(ind, ind + m, Tok::Keyword);
            i = ind + m;
        }
    }
    let first = o.v.as_ref().map_or(0, |v| v.len());
    md_inline(l, i, o, &mut st);
    if let Some(v) = o.v.as_mut() {
        uncross(v, first);
    }
    st
}

/// Drops the spans of `v[from..]` that cross another (emphasis is looked for on its own: `*a [b* c](d)`), so no
/// byte's color depends on which of two came last: emphasis first, then a link's, and of two alike the later one.
fn uncross(v: &mut Vec<Span>, from: usize) {
    if v.len() < from + 2 {
        return;
    }
    let rank = |t: Tok| match t {
        Tok::Bold | Tok::Italic => 0,
        Tok::Link | Tok::Dim => 1,
        _ => 2,
    };
    let spans = &mut v[from..];
    // (the outer of two nested spans first, as the inner one's color goes over it)
    spans.sort_unstable_by_key(|s| (s.0, std::cmp::Reverse(s.1)));
    // the spans the one looked at is inside of, innermost last; a dropped one is made empty
    let (mut open, mut depth) = ([0usize; 8], 0);
    for k in 0..spans.len() {
        let (s, e, tok) = spans[k];
        let mut keep = true;
        while depth > 0 {
            let top = open[depth - 1];
            if spans[top].1 <= s {
                depth -= 1;
            } else if spans[top].1 >= e {
                break;
            } else if rank(tok) <= rank(spans[top].2) {
                keep = false;
                break;
            } else {
                spans[top].1 = spans[top].0;
                depth -= 1;
            }
        }
        if keep && depth < open.len() {
            open[depth] = k;
            depth += 1;
        } else {
            spans[k].1 = s;
        }
    }
    let mut w = from;
    for k in from..v.len() {
        if v[k].0 < v[k].1 {
            v[w] = v[k];
            w += 1;
        }
    }
    v.truncate(w);
}

/// How far ahead the end of `code`, a link's (url) or a <tag> is looked for.
const MD_LOOK: usize = 2048;
/// How far an inline code span's closing backticks are looked for (unmatched ones on a long line add up).
const MD_CODE_LOOK: usize = 512;
/// The same for *emphasis* and a [link's text], which are rarely longer than a sentence (a line of `*a *b *c…`
/// looked 2 KB ahead for each star).
const SHORT_LOOK: usize = 256;

/// An inline code span whose backticks start at `i` (`code`, ``co`de``), its closing run looked for before `look`:
/// the length of the opening run, and where the span ends (None: the backticks are just characters).
fn code_span(l: &[u8], i: usize, look: usize) -> (usize, Option<usize>) {
    let run = l[i..].iter().take_while(|&&b| b == b'`').count();
    let mut p = i + run;
    while let Some(q) = memchr::memchr(b'`', &l[p.min(look)..look]) {
        let s = p + q;
        let r = l[s..].iter().take_while(|&&b| b == b'`').count();
        if r == run {
            return (run, Some(s + r));
        }
        p = s + r;
    }
    (run, None)
}

/// The closing run of `k` `c` characters for emphasis opened before `from` (not one in `code`: code goes first).
fn emphasis_close(l: &[u8], from: usize, c: u8, k: usize) -> Option<usize> {
    let end = l.len().min(from + SHORT_LOOK);
    let mut p = from;
    while let Some(q) = memchr::memchr2(c, b'`', &l[p.min(end)..end]) {
        let s = p + q;
        if l[s] == b'`' {
            let (run, e) = code_span(l, s, end);
            p = e.unwrap_or(s + run);
            continue;
        }
        if s + k <= l.len()
            && l[s..s + k].iter().all(|&b| b == c)
            && s > from
            && !l[s - 1].is_ascii_whitespace()
            && !(c == b'_' && at(l, s + k).is_ascii_alphanumeric())
        {
            return Some(s);
        }
        p = s + 1;
    }
    None
}

/// Inline Markdown. Only code spans, HTML comments and tags hide what's inside them, so only they matter for the
/// state; emphasis, links and URLs are just colored (and skipped when only the state is wanted).
fn md_inline(l: &[u8], mut i: usize, o: &mut Out, st: &mut State) {
    let n = l.len();
    let full = o.on();
    // a link's own (url) isn't colored again as a bare URL
    let mut url_from = 0;
    while i < n {
        let c = l[i];
        match c {
            b'\\' => i += 2,
            b'`' => match code_span(l, i, n.min(i + MD_CODE_LOOK)) {
                (_, Some(e)) => {
                    o.put(i, e, Tok::Str);
                    i = e;
                }
                (run, None) => i += run,
            },
            b'<' => {
                if l[i..].starts_with(b"<!--") {
                    match find(l, i + 4, b"-->") {
                        Some(p) => {
                            o.put(i, p + 3, Tok::Comment);
                            i = p + 3;
                        }
                        None => {
                            o.put(i, n, Tok::Comment);
                            st.kind = HTML_COMMENT;
                            return;
                        }
                    }
                    continue;
                }
                // <tag> or <https://autolink> (only these start with a letter, digit or `/`: `<<<` isn't looked into)
                if !l.get(i + 1).is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'/') {
                    i += 1;
                    continue;
                }
                match memchr::memchr(b'>', &l[i..n.min(i + MD_LOOK)]) {
                    Some(p) if p > 1 => {
                        let inner = &l[i + 1..i + p];
                        let link = inner.starts_with(b"http") || inner.starts_with(b"mailto:") || (inner.contains(&b'@') && !inner.contains(&b' '));
                        if link || inner[0].is_ascii_alphabetic() || inner[0] == b'/' {
                            o.put(i, i + p + 1, if link { Tok::Link } else { Tok::Tag });
                            i += p + 1;
                            continue;
                        }
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            _ if !full => i += 1,
            b'*' | b'_' => {
                let run = l[i..].iter().take_while(|&&b| b == c).count();
                let k = run.min(2);
                let next = at(l, i + run);
                let opens = next != 0 && !next.is_ascii_whitespace() && !(c == b'_' && i > 0 && l[i - 1].is_ascii_alphanumeric());
                if opens {
                    if let Some(p) = emphasis_close(l, i + k, c, k) {
                        o.put(i, p + k, if k == 2 { Tok::Bold } else { Tok::Italic });
                    }
                }
                // what's inside is still looked at (code, comments)
                i += run;
            }
            b'[' | b'!' if c == b'[' || at(l, i + 1) == b'[' => {
                // [text](url), ![image](src), [text][ref]
                let s = if c == b'!' { i + 1 } else { i };
                let look = n.min(s + SHORT_LOOK);
                let mut depth = 0;
                let mut close = None;
                for (k, &b) in l[s..look].iter().enumerate() {
                    match b {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(s + k);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(cl) = close {
                    let look = n.min(cl + 1 + MD_LOOK);
                    let end = match at(l, cl + 1) {
                        b'(' => memchr::memchr(b')', &l[cl + 1..look]).map(|p| cl + 2 + p),
                        b'[' => memchr::memchr(b']', &l[(cl + 2).min(look)..look]).map(|p| cl + 3 + p),
                        _ => None,
                    };
                    if let Some(e) = end {
                        o.put(i, cl + 1, Tok::Link);
                        o.put(cl + 1, e, Tok::Dim);
                        url_from = e;
                    }
                }
                // (an image's `[` is looked at already)
                i = s + 1;
            }
            b'h' if i >= url_from
                && (l[i..].starts_with(b"http://") || l[i..].starts_with(b"https://"))
                && (i == 0 || !l[i - 1].is_ascii_alphanumeric()) =>
            {
                let e = i + l[i..]
                    .iter()
                    .take_while(|&&b| !b.is_ascii_whitespace() && !matches!(b, b')' | b'>' | b'"' | b'<' | b'`' | b'\\'))
                    .count();
                o.put(i, e, Tok::Link);
                i = e;
            }
            b'|' => {
                o.put(i, i + 1, Tok::Punct);
                i += 1;
            }
            _ => i += 1,
        }
    }
}

// ---- YAML ----

/// Inside a block scalar (`key: |` / `key: >`); `b`: the indentation of the line that started it.
const BLOCK: u8 = 1;
/// Inside a quoted scalar that goes on over lines; `a`: its quote, `b`: an escape pending (bit 0), the indentation
/// of what it belongs to (bits 1-7) and the lines so far (bits 8-15).
const QUOTED: u8 = 2;
/// A quoted scalar left open gives up after this many lines.
const MAX_QUOTED_LINES: u16 = 40;

pub(super) fn yaml(t: &[u8], mut st: State, o: &mut Out) -> State {
    let mut start = 0;
    let mut col0 = st.col0;
    loop {
        let end = line_end(t, start);
        let nl = end < t.len();
        st = yaml_line(&t[start..end], nl, col0, st, &mut o.at(start));
        if !nl {
            return st;
        }
        start = end + 1;
        col0 = true;
    }
}

/// Where a quoted scalar (`'` doubles itself inside, `"` has escapes) that is open at `i` closes on this line.
fn yaml_quote_end(l: &[u8], mut i: usize, q: u8, esc: &mut bool) -> Option<usize> {
    while i < l.len() {
        let c = l[i];
        if *esc {
            *esc = false;
        } else if c == b'\\' && q == b'"' {
            *esc = true;
        } else if c == q {
            if q == b'\'' && at(l, i + 1) == b'\'' {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// A quoted scalar from `s` (its body from `i`); if it doesn't close on this line, the state it goes on in
/// (`parent`: the indentation of the key or item it belongs to; `nl`: the line ends here, which uses up an escape).
#[allow(clippy::too_many_arguments)]
fn yaml_quoted(l: &[u8], s: usize, i: usize, q: u8, mut esc: bool, parent: u16, lines: u16, nl: bool, st: &mut State, o: &mut Out) -> Option<usize> {
    let n = l.len();
    match yaml_quote_end(l, i, q, &mut esc) {
        Some(e) => {
            o.put(s, e, Tok::Str);
            *st = State { kind: 0, a: 0, b: 0, ..*st };
            Some(e)
        }
        None => {
            o.put(s, n, Tok::Str);
            let lines = lines + nl as u16;
            *st = if lines >= MAX_QUOTED_LINES {
                State { kind: 0, a: 0, b: 0, ..*st }
            } else {
                State { kind: QUOTED, a: q, b: (esc && !nl) as u16 | parent.min(127) << 1 | lines << 8, ..*st }
            };
            None
        }
    }
}

fn yaml_line(l: &[u8], nl: bool, col0: bool, mut st: State, o: &mut Out) -> State {
    let n = l.len();
    let ind = if col0 { l.iter().take_while(|&&b| b == b' ').count() } else { 0 };
    if st.kind == QUOTED {
        let parent = (st.b >> 1 & 127) as usize;
        // a new key or item no further in than the one it belongs to: it was left open by mistake
        let rest = &l[ind..];
        let ended = col0
            && ind <= parent
            && (yaml_key_end(l, ind).is_some() || rest.starts_with(b"- ") || rest.starts_with(b"#") || rest.starts_with(b"---"));
        if !ended {
            let Some(e) = yaml_quoted(l, 0, 0, st.a, st.b & 1 != 0, parent as u16, st.b >> 8, nl, &mut st, o) else { return st };
            if let Some(p) = l[e..].iter().position(|&b| b == b'#') {
                o.put(e + p, n, Tok::Comment);
            }
            return st;
        }
        st = State { kind: 0, a: 0, b: 0, ..st };
    }
    if st.kind == BLOCK {
        if !col0 || l.trim_ascii().is_empty() || ind > st.b as usize {
            o.put(0, n, Tok::Str);
            return st;
        }
        // less indented: the block scalar is over
        st = State { kind: 0, a: 0, b: 0, ..st };
    }
    if !col0 {
        yaml_value(l, 0, 0, nl, o, &mut st);
        // (where its key is isn't known here)
        if st.kind == BLOCK {
            st.kind = 0;
        }
        return st;
    }
    let mut i = ind;
    // what a block scalar on this line belongs to: the key, or the list item
    let mut parent = ind;
    let rest = &l[i..];
    if at(rest, 0) == b'#' {
        o.put(i, n, Tok::Comment);
        return st;
    }
    if i == 0 && (rest.starts_with(b"---") || rest.starts_with(b"...")) && matches!(at(rest, 3), 0 | b' ' | b'\t' | b'\r') {
        o.put(0, 3, Tok::Punct);
        yaml_value(l, 3, ind, nl, o, &mut st);
        return st;
    }
    if i == 0 && at(rest, 0) == b'%' {
        o.put(0, n, Tok::Control);
        return st;
    }
    // "- " list items (possibly "- - x")
    while at(l, i) == b'-' && matches!(at(l, i + 1), 0 | b' ' | b'\t' | b'\r') {
        o.put(i, i + 1, Tok::Punct);
        parent = i;
        i += 1;
        i += l[i..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
    }
    if let Some(colon) = yaml_key_end(l, i) {
        o.put(i, colon, Tok::Key);
        o.put(colon, colon + 1, Tok::Punct);
        parent = i;
        i = colon + 1;
    }
    yaml_value(l, i, parent, nl, o, &mut st);
    st
}

/// The ':' ending a mapping key that starts at `i` (it must be followed by a space or the end of the line).
fn yaml_key_end(l: &[u8], i: usize) -> Option<usize> {
    let c = at(l, i);
    let sep = |p: usize| matches!(at(l, p), 0 | b' ' | b'\t' | b'\r');
    if c == b'"' || c == b'\'' {
        let (e, closed, _) = scan_str(l, i + 1, c, if c == b'"' { b'\\' } else { 0 }, false, true);
        return (closed && at(l, e) == b':' && sep(e + 1)).then_some(e);
    }
    if c == 0 || matches!(c, b'[' | b'{' | b'#' | b'|' | b'>' | b'&' | b'*' | b'!' | b'%' | b'@' | b'`') {
        return None;
    }
    let mut j = i;
    while j < l.len() {
        match l[j] {
            b':' if sep(j + 1) => return Some(j),
            b'#' if matches!(l[j - 1], b' ' | b'\t') => return None,
            _ => j += 1,
        }
    }
    None
}

fn yaml_scalar_tok(v: &[u8]) -> Tok {
    let lower = v.to_ascii_lowercase();
    if is_num_word(v) || matches!(lower.as_slice(), b".inf" | b"-.inf" | b".nan") {
        Tok::Num
    } else if matches!(lower.as_slice(), b"true" | b"false" | b"yes" | b"no" | b"on" | b"off" | b"null" | b"~") {
        Tok::Lit
    } else {
        Tok::Str
    }
}

fn yaml_value(l: &[u8], mut i: usize, ind: usize, nl: bool, o: &mut Out, st: &mut State) {
    let n = l.len();
    i += l[i.min(n)..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
    if i >= n {
        return;
    }
    let c = l[i];
    match c {
        b'#' => o.put(i, n, Tok::Comment),
        b'|' | b'>' => {
            // block scalar header: |, >-, |+2 ...
            let j = i + 1 + l[i + 1..].iter().take_while(|&&b| matches!(b, b'+' | b'-' | b'0'..=b'9')).count();
            let after = l[j..].trim_ascii_start();
            if after.is_empty() || after[0] == b'#' {
                o.put(i, j, Tok::Punct);
                o.put(n - after.len(), n, Tok::Comment);
                *st = State { kind: BLOCK, a: 0, b: ind as u16, ..*st };
            } else {
                o.put(i, n, Tok::Str);
            }
        }
        b'"' | b'\'' => {
            // (it may go on over lines)
            let Some(e) = yaml_quoted(l, i, i + 1, c, false, ind as u16, 0, nl, st, o) else { return };
            if let Some(p) = l[e..].iter().position(|&b| b == b'#') {
                o.put(e + p, n, Tok::Comment);
            }
        }
        b'&' | b'*' | b'!' => {
            // &anchor, *alias, !tag
            let e = i + l[i..].iter().take_while(|&&b| !matches!(b, b' ' | b'\t' | b',' | b']' | b'}')).count();
            o.put(i, e, if c == b'!' { Tok::Type } else { Tok::Var });
            yaml_value(l, e, ind, nl, o, st);
        }
        b'[' | b'{' => {
            // flow collections: [a, "b", 3], {k: v}
            while i < n {
                let b = l[i];
                match b {
                    b'[' | b']' | b'{' | b'}' | b',' | b':' => {
                        o.put(i, i + 1, Tok::Punct);
                        i += 1;
                    }
                    b'"' | b'\'' => {
                        let (e, _, _) = scan_str(l, i + 1, b, if b == b'"' { b'\\' } else { 0 }, false, true);
                        o.put(i, e, Tok::Str);
                        i = e;
                    }
                    b'#' if i > 0 && matches!(l[i - 1], b' ' | b'\t') => {
                        o.put(i, n, Tok::Comment);
                        return;
                    }
                    b' ' | b'\t' => i += 1,
                    _ => {
                        // (`:` ends it too: looking for it afterwards made `[a:a:a…` quadratic)
                        let e = i + l[i..].iter().take_while(|&&b| !matches!(b, b',' | b']' | b'}' | b'[' | b'{' | b':')).count();
                        let e = e.max(i + 1);
                        let v = l[i..e].trim_ascii_end();
                        o.put(i, i + v.len(), yaml_scalar_tok(v));
                        i = e;
                    }
                }
            }
        }
        _ => {
            // a plain scalar, to a " #" comment or the end of the line
            let mut e = i;
            while e < n && !(l[e] == b'#' && matches!(l[e - 1], b' ' | b'\t')) {
                e += 1;
            }
            let v = l[i..e].trim_ascii_end();
            o.put(i, i + v.len(), yaml_scalar_tok(v));
            if e < n {
                o.put(e, n, Tok::Comment);
            }
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
    fn doctype_subsets() {
        // in the subset, comments, PIs and quoted values hide brackets and `>`
        let src = "<!DOCTYPE c [\n  <!-- the c's [elements] > -->\n  <?pi don't ]>?>\n  <!ENTITY e \"]>\">\n]>\n<c a='1'/>\n";
        let v = view(Lang::Xml, src);
        assert!(line_has(&v[1], "<!-- the c's [elements] > -->", Tok::Comment));
        assert!(line_has(&v[2], "<?pi don't ]>?>", Tok::Section));
        assert!(line_has(&v[3], "  <!ENTITY e \"]>\">", Tok::Keyword));
        assert!(line_has(&v[5], "c", Tok::Tag) && line_has(&v[5], "'1'", Tok::Str));
        assert_eq!(end_state(Lang::Xml, src), State::START);
    }

    #[test]
    fn markdown_code_blocks_in_their_language() {
        let src = "Text\n```rust\nlet s = \"a\"; /* open\nstill */ fn f() {}\n```\n# After\n";
        let v = view(Lang::Markdown, src);
        assert!(line_has(&v[2], "let", Tok::Keyword) && line_has(&v[2], "\"a\"", Tok::Str) && line_has(&v[2], "/* open", Tok::Comment));
        assert!(line_has(&v[3], "still */", Tok::Comment) && line_has(&v[3], "f", Tok::Func));
        assert_eq!(v[4], vec![("```".into(), Tok::Punct)]);
        assert_eq!(v[5], vec![("# After".into(), Tok::Heading)]);
        assert_eq!(end_state(Lang::Markdown, src), State::START);
        // a language whose state doesn't fit (HTML inside a script) goes on line by line
        let v = view(Lang::Markdown, "~~~html\n<script>\nlet x = 1;\n</script>\n~~~\nok *x*\n");
        assert!(line_has(&v[1], "script", Tok::Tag) && line_has(&v[3], "script", Tok::Tag));
        assert!(line_has(&v[4], "~~~", Tok::Punct) && line_has(&v[5], "*x*", Tok::Italic));
        // longer fences too, from each line's start
        let v = view(Lang::Markdown, "````python\n'''doc\n```\n````\nafter\n");
        assert!(line_has(&v[1], "'''doc", Tok::Str) && !line_has(&v[2], "```", Tok::Punct) && line_has(&v[3], "````", Tok::Punct));
        // an unknown language looks as before; PHP snippets are PHP code without `<?php`
        assert_eq!(view(Lang::Markdown, "```foo\nx = 1\n```\n")[1], vec![("x = 1".into(), Tok::Str)]);
        let v = view(Lang::Markdown, "```php\n$x = 'a'; // c\n```\n");
        assert!(line_has(&v[1], "$x", Tok::Var) && line_has(&v[1], "'a'", Tok::Str) && line_has(&v[1], "// c", Tok::Comment));
        let v = view(Lang::Markdown, "```{r}\nx <- c(1, NA)\n```\n");
        assert!(line_has(&v[1], "NA", Tok::Lit));
    }

    #[test]
    fn yaml_quoted_scalars_over_lines() {
        let v = view(Lang::Yaml, "msg: \"first\n  second # not a comment\n  third\" # c\nnext: 1\n");
        assert_eq!(v[1], vec![("  second # not a comment".into(), Tok::Str)]);
        assert!(line_has(&v[2], "  third\"", Tok::Str) && line_has(&v[2], "# c", Tok::Comment) && line_has(&v[3], "next", Tok::Key));
        has(Lang::Yaml, "a: 'it''s' # c", &[("'it''s'", Tok::Str), ("# c", Tok::Comment)]);
        // left open by mistake: over at the next key that isn't further in
        let v = view(Lang::Yaml, "title: \"Hello\nauthor: Bob\n");
        assert!(line_has(&v[1], "author", Tok::Key));
        assert_eq!(end_state(Lang::Yaml, "title: \"Hello\nauthor: Bob\n").kind, 0);
    }

    #[test]
    fn xml_and_html() {
        has(Lang::Xml, r#"<?xml version="1.0"?><root a="1" b='x>y'><!-- note --><item/>&amp;<![CDATA[<raw>]]></root>"#, &[
            (r#"<?xml version="1.0"?>"#, Tok::Section),
            ("root", Tok::Tag),
            ("a", Tok::Attr),
            ("\"1\"", Tok::Str),
            ("'x>y'", Tok::Str),
            ("<!-- note -->", Tok::Comment),
            ("&amp;", Tok::Lit),
            ("<![CDATA[<raw>]]>", Tok::Str),
        ]);
        // a tag and a comment spread over lines
        let st = end_state(Lang::Xml, "<config\n  name=\"a\n");
        let t = toks(Lang::Xml, "b\" port='80'>text", st);
        assert_eq!(t[0], ("b\"".into(), Tok::Str));
        assert!(t.contains(&("port".into(), Tok::Attr)));
        let st = end_state(Lang::Xml, "<a><!-- start\n");
        assert_eq!(toks(Lang::Xml, "end --><b>", st)[0], ("end -->".into(), Tok::Comment));
        // script and style inside HTML
        has(Lang::Html, "<script>if (x) { y = \"<b>\"; }</script><style>p { color: red }</style>", &[
            ("if", Tok::Control),
            ("\"<b>\"", Tok::Str),
            ("color", Tok::Attr),
        ]);
        let st = end_state(Lang::Html, "<SCRIPT type=module>\nlet a = 1;\n");
        let t = toks(Lang::Html, "/* c */ </script> <p>", st);
        assert_eq!(t[0], ("/* c */".into(), Tok::Comment));
        assert!(t.contains(&("script".into(), Tok::Tag)));
        assert!(t.contains(&("p".into(), Tok::Tag)));
    }

    #[test]
    fn markdown_blocks_and_inline() {
        has(Lang::Markdown, "# Title", &[("# Title", Tok::Heading)]);
        has(Lang::Markdown, "- an **important** and *nice* `code` [link](http://x.y) item", &[
            ("-", Tok::Keyword),
            ("**important**", Tok::Bold),
            ("*nice*", Tok::Italic),
            ("`code`", Tok::Str),
            ("[link]", Tok::Link),
            ("(http://x.y)", Tok::Dim),
        ]);
        has(Lang::Markdown, "> quoted", &[("> quoted", Tok::Comment)]);
        has(Lang::Markdown, "| a | b |", &[("|", Tok::Punct)]);
        has(Lang::Markdown, "snake_case_name stays plain", &[]);
        assert!(toks(Lang::Markdown, "snake_case_name", super::super::State::START).is_empty());
        // fenced code blocks span lines (colored as their language)
        let st = end_state(Lang::Markdown, "Text\n```python\nx = 1  # *not* emphasis\n");
        assert_eq!(toks(Lang::Markdown, "y = 2", st), vec![("2".into(), Tok::Num)]);
        assert_eq!(toks(Lang::Markdown, "# *x*", st), vec![("# *x*".into(), Tok::Comment)]);
        let st = end_state(Lang::Markdown, "```\ncode\n```\n");
        assert_eq!(toks(Lang::Markdown, "# After", st), vec![("# After".into(), Tok::Heading)]);
        // a star in code closes nothing, a closing star opens nothing more, an image's link is colored once
        has(Lang::Markdown, "*all `*.log` files*", &[("*all `*.log` files*", Tok::Italic), ("`*.log`", Tok::Str)]);
        assert_eq!(toks(Lang::Markdown, "*a*b*", State::START), vec![("*a*".into(), Tok::Italic)]);
        assert_eq!(toks(Lang::Markdown, "**a**b**c**", State::START), vec![("**a**".into(), Tok::Bold), ("**c**".into(), Tok::Bold)]);
        assert_eq!(toks(Lang::Markdown, "![logo](x.png)", State::START), vec![("![logo]".into(), Tok::Link), ("(x.png)".into(), Tok::Dim)]);
        // what still crosses is dropped, emphasis first
        assert_eq!(toks(Lang::Markdown, "*a [b* c](d)", State::START), vec![("[b* c]".into(), Tok::Link), ("(d)".into(), Tok::Dim)]);
        // emphasis or links that never close cost little to look for
        let start = std::time::Instant::now();
        for p in ["*a ", "#["] {
            toks(Lang::Markdown, &p.repeat(300_000), super::super::State::START);
        }
        assert!(start.elapsed().as_secs_f64() < 2.0, "{:?}", start.elapsed());
    }

    #[test]
    fn yaml_documents() {
        has(Lang::Yaml, "name: \"Slate\" # app", &[("name", Tok::Key), ("\"Slate\"", Tok::Str), ("# app", Tok::Comment)]);
        has(Lang::Yaml, "  - port: 8080", &[("-", Tok::Punct), ("port", Tok::Key), ("8080", Tok::Num)]);
        has(Lang::Yaml, "enabled: true", &[("true", Tok::Lit)]);
        has(Lang::Yaml, "url: http://x.y:80/a", &[("url", Tok::Key), ("http://x.y:80/a", Tok::Str)]);
        has(Lang::Yaml, "base: &b {a: 1, b: [x, \"y\"]}", &[("&b", Tok::Var), ("1", Tok::Num), ("\"y\"", Tok::Str)]);
        // block scalars (Home Assistant templates) span lines until the indentation drops
        let st = end_state(Lang::Yaml, "sensor:\n  value_template: >\n    {{ states('x') }}\n");
        assert_eq!(toks(Lang::Yaml, "    and: more", st), vec![("    and: more".into(), Tok::Str)]);
        assert!(toks(Lang::Yaml, "  next: 1", st).contains(&("next".into(), Tok::Key)));
        // in a list item the block belongs to the key: the item's next key ends it
        let st = end_state(Lang::Yaml, "steps:\n  - run: |\n      cargo test\n");
        assert_eq!(toks(Lang::Yaml, "      more", st), vec![("      more".into(), Tok::Str)]);
        assert!(toks(Lang::Yaml, "    env:", st).contains(&("env".into(), Tok::Key)));
    }
}
