//! What File → Open selected file or link opens (issue #7, both apps): a web link in the browser, or a file named in
//! the text in a tab of its own. Never a program: a name is only ever opened as a text file.

use std::path::{Component, Path, PathBuf};

use crate::core::document::{Document, Sel};

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// An http or https address, for the browser.
    Link(String),
    /// A file (a full path), and the line and column to go to when the name ended in `:line` or `:line:column`.
    File(PathBuf, Option<(u64, u64)>),
}

/// Longest selection taken as a name.
const MAX: usize = 2048;

/// The file name or link around byte `col` of `line` (with nothing selected): a link there (brackets in it kept,
/// `…/Slate_(disambiguation)`), else a quoted name the place is inside (`"sub folder\with space.txt"`), else the run
/// of characters there that has no spaces, quotes or brackets in it; without punctuation that ends a sentence (or a
/// colon) after it. Empty if there's none.
pub fn around(line: &str, col: usize) -> &str {
    let col = (0..=col.min(line.len())).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
    if let Some(url) = link_around(line, col) {
        return url;
    }
    if let Some(q) = quoted_around(line, col) {
        return q;
    }
    let stop = |c: char| c.is_whitespace() || "\"'`<>()[]{}|“”‘’«»".contains(c);
    let a = line[..col].rfind(stop).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let b = line[col..].find(stop).map_or(line.len(), |i| col + i);
    line[a..b].trim_end_matches(['.', ',', ';', '!', '?', ':'])
}

/// A link (http, https, www.) the place `col` is in: up to a space or a quote, brackets in it kept when they pair up
/// (a closing one more than were opened ends it, as around a link in brackets), and punctuation after it left out.
fn link_around(line: &str, col: usize) -> Option<&str> {
    let edge = |c: char| c.is_whitespace() || "\"'`<>“”‘’«»".contains(c);
    let a = line[..col].rfind(edge).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let token = &line[a..];
    let lower = token.to_ascii_lowercase();
    // (the link's start in the token: it may follow a bracket, `(https://…`)
    let start = ["https://", "http://", "www."].iter().filter_map(|s| lower.find(s)).min()?;
    if a + start > col {
        return None;
    }
    let mut depth = 0i32;
    let mut end = token.len();
    // (a host in brackets, `https://[::1]:8080/`, is part of it)
    let host_at = token[start..].find("://").map_or(0, |i| i + 3);
    let bracket_end = token[start + host_at..].starts_with('[').then(|| token[start + host_at..].find(']')).flatten().map(|i| host_at + i);
    for (i, c) in token[start..].char_indices() {
        if bracket_end.is_some_and(|e| i >= host_at && i <= e) {
            continue;
        }
        if edge(c) || "[]{}|".contains(c) {
            end = start + i;
            break;
        }
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = start + i;
                break;
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    let url = token[start..end].trim_end_matches(['.', ',', ';', '!', '?', ':']);
    (a + start + url.len() >= col && !url.is_empty()).then_some(url)
}

/// The text between the quotes the place `col` is inside, on its line, when it looks like a name with a space in it
/// (one without is `around`'s anyway).
fn quoted_around(line: &str, col: usize) -> Option<&str> {
    for q in ['"', '\''] {
        let Some(a) = line[..col].rfind(q) else { continue };
        let Some(b) = line[col..].find(q) else { continue };
        let inner = &line[a + 1..col + b];
        // (an odd number of quotes before: the place is inside a pair)
        if line[..a].matches(q).count() % 2 == 0 && inner.contains(' ') && !inner.trim().is_empty() && plausible(inner.trim()) {
            return Some(inner.trim());
        }
    }
    None
}

/// What Open selected file or link opens in `doc`, whose selection is `sel`: at `at` (a right-click's place) the
/// selection if `at` is in it, else the name around `at`; with no `at`, the selection, or the name around the caret.
/// A relative name is in the folder of the document's file. `names_only` (the text's menu, which offers it unasked):
/// a selection too only if it looks like a name (`plausible`), not any word that happens to be selected.
pub fn target_in(doc: &Document, sel: Sel, at: Option<u64>, names_only: bool) -> Result<Target, String> {
    let inside = at.is_none_or(|p| !sel.is_empty() && sel.start() <= p && p <= sel.end());
    let text = if !inside || sel.is_empty() {
        let pos = at.unwrap_or(sel.caret).min(doc.len());
        let (ls, le) = (doc.line_start_of(pos), doc.line_end_of(pos));
        let (a, b) = (pos.saturating_sub(2048).max(ls), (pos + 2048).min(le));
        let line = String::from_utf8_lossy(&doc.read(a, b)).into_owned();
        let name = around(&line, (pos - a) as usize);
        if !plausible(name) {
            return Err("Select a file name or a link first".into());
        }
        // (a compiler's place right after it: `main.rs(40,5)`)
        let end = (name.as_ptr() as usize).saturating_sub(line.as_ptr() as usize) + name.len();
        match line.get(end..).and_then(|rest| rest.find(')').map(|i| &rest[..=i])) {
            Some(p) if p.starts_with('(') && paren_suffix(&format!("x{p}")).is_some() => format!("{name}{p}"),
            _ => name.to_string(),
        }
    } else if sel.end() - sel.start() > MAX as u64 {
        return Err("Select one file name or link".into());
    } else {
        let text = String::from_utf8_lossy(&doc.read(sel.start(), sel.end())).into_owned();
        if names_only && !plausible(text.trim()) {
            return Err("Select a file name or a link first".into());
        }
        text
    };
    target_of(&text, doc.path.as_deref().and_then(Path::parent))
}

/// The menu item for `t`: `Open “name”`, or `Open link “example.com”` (`&`: Windows' letter). A file outside
/// `folder` (the tab's file's) shows its whole path, so the item says where it goes (`..\..`, a network share,
/// `%NAME%`); a link its site.
pub fn menu_label(t: &Target, folder: Option<&Path>, amp: bool) -> String {
    let o = if amp { "&Open" } else { "Open" };
    let text = match t {
        Target::Link(url) => {
            let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
            let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            return format!("{o} link “{}”", if amp { host.replace('&', "&&") } else { host.to_string() });
        }
        Target::File(p, _) if folder.is_some_and(|f| p.starts_with(f)) => {
            p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
        }
        Target::File(p, _) => {
            let s = p.to_string_lossy().into_owned();
            // (a long path: its start and end)
            if s.chars().count() > 70 { format!("{}…{}", s.chars().take(30).collect::<String>(), s.chars().skip(s.chars().count() - 38).collect::<String>()) } else { s }
        }
    };
    format!("{o} “{}”", if amp { text.replace('&', "&&") } else { text })
}

/// Whether a name from around the caret (`around`, nothing selected) looks like a file name or a link: it's a link, or
/// has a folder in it, or an ending with a letter in it (`notes.txt`, `main.rs:40`). A word alone (`gamma`), a number
/// (`1.5`), `e.g`, an abbreviation (`U.S.A`), two sentences without their space (`end.Then`) or a mail address isn't
/// one. (With `names_only`, a selection is held to the same; else it's taken as it is.) The price: a dot-file
/// (`.env`) or an ending with a capital (`notes.Txt`) isn't offered at the caret; selected, File → Open selected file
/// or link still opens it.
pub fn plausible(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") || lower.starts_with("file://") {
        return true;
    }
    let base = paren_suffix(s).or_else(|| line_suffix(s)).map_or(s, |(b, _, _)| b);
    let folder = base.contains(['/', '\\']);
    if !folder && base.contains('@') {
        return false;
    }
    let ending = base.rsplit_once('.').is_some_and(|(stem, ext)| {
        let titled = ext.len() >= 3 && ext.starts_with(|c: char| c.is_ascii_uppercase()) && ext[1..].chars().all(|c| c.is_ascii_lowercase());
        let abbreviation = stem.split('.').all(|p| p.chars().count() == 1) && ext.chars().count() == 1;
        !stem.is_empty()
            && (stem.len() >= 2 || ext.len() >= 2)
            && ext.len() <= 10
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
            && !titled
            && !abbreviation
    });
    ending || folder
}

/// What `text` names (a selection, or what `around` gave): a link, or a file, a relative name being in `folder` (the
/// folder of the tab's file). Err says why it's neither.
pub fn target_of(text: &str, folder: Option<&Path>) -> Result<Target, String> {
    let mut t = text.trim();
    for (a, b) in [('"', '"'), ('\'', '\''), ('<', '>'), ('(', ')'), ('`', '`')] {
        if t.len() >= 2 && t.starts_with(a) && t.ends_with(b) {
            t = t[1..t.len() - 1].trim();
            break;
        }
    }
    if t.is_empty() {
        return Err("Select a file name or a link first".into());
    }
    if t.len() > MAX || t.contains(['\n', '\r']) {
        return Err("Select one file name or link".into());
    }
    if t.chars().any(char::is_control) {
        return Err("That isn't a file name or a link".into());
    }
    let lower = t.to_ascii_lowercase();
    // (`file:///C:/notes/a%20b.txt`, `file:///home/x`: the file it names)
    if let Some(rest) = lower.starts_with("file://").then(|| &t["file://".len()..]) {
        let path = percent_decoded(rest.strip_prefix("localhost").unwrap_or(rest));
        let path = if cfg!(windows) && path.len() > 2 && path.starts_with('/') && path.as_bytes()[2] == b':' { path[1..].to_string() } else { path };
        let p = PathBuf::from(local(&path));
        return if p.is_absolute() { Ok(Target::File(tidy(&p), None)) } else { Err("That isn't a file name or a link".into()) };
    }
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        if t.contains(char::is_whitespace) {
            return Err("A link has no spaces in it".into());
        }
        let url = if lower.starts_with("www.") { format!("https://{t}") } else { t.to_string() };
        // (an address needs a site: not `https://` alone)
        let rest = url.split_once("://").map_or("", |(_, r)| r);
        if rest.split(['/', '?', '#']).next().is_none_or(str::is_empty) {
            return Err("That link has no address".into());
        }
        return Ok(Target::Link(url));
    }
    // (another kind of address, `mailto:`, `ms-settings:`: not for Slate to open; `C:\…` is a drive, `notes:12` a
    // line)
    if let Some((scheme, rest)) = t.split_once(':') {
        let word = scheme.len() > 1 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-');
        if word && !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return Err("Slate opens web links (http and https) and file names".into());
        }
    }
    let (name, at) = match paren_suffix(t).or_else(|| line_suffix(t)) {
        Some((base, line, col)) => (base, Some((line, col))),
        None => (t, None),
    };
    let name = local(name);
    let p = PathBuf::from(&name);
    let p = if p.is_absolute() {
        p
    } else {
        match folder {
            Some(f) => f.join(p),
            None => return Err("Save this file first, or select a full path".into()),
        }
    };
    Ok(Target::File(tidy(&p), at))
}

/// `name:120` or `name:120:5`: the name, and the line and column (1 when there's none).
fn line_suffix(t: &str) -> Option<(&str, u64, u64)> {
    let number = |s: &str| (!s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    let (rest, last) = t.rsplit_once(':')?;
    let last = number(last)?;
    let (base, line, col) = match rest.rsplit_once(':').and_then(|(b, l)| Some((b, number(l)?))) {
        Some((base, line)) => (base, line, last),
        None => (rest, last, 1),
    };
    // (not a drive's colon: `C:5` isn't line 5 of C)
    if base.is_empty() || (base.len() == 1 && base.chars().all(|c| c.is_ascii_alphabetic())) {
        return None;
    }
    Some((base, line.max(1), col.max(1)))
}

/// `main.rs(40,5)` or `main.rs(40)`, as compilers write them: the name, and the line and column (1 when there's none).
fn paren_suffix(t: &str) -> Option<(&str, u64, u64)> {
    let (base, inner) = t.strip_suffix(')')?.rsplit_once('(')?;
    let number = |s: &str| (!s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    let (line, col) = match inner.split_once(',') {
        Some((l, c)) => (number(l.trim())?, number(c.trim())?),
        None => (number(inner.trim())?, 1),
    };
    (!base.is_empty()).then_some((base, line.max(1), col.max(1)))
}

/// `%20` and the like made the bytes they stand for (a `file:` address).
fn percent_decoded(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A name as this system writes it: `/` or `\` the separator it uses; `~/` (or `~\` on Windows) the home folder; on
/// Windows `%NAME%` (an environment variable that is set) replaced by its value.
fn local(name: &str) -> String {
    if cfg!(windows) {
        let name = match (name.strip_prefix("~\\").or_else(|| name.strip_prefix("~/")), std::env::var_os("USERPROFILE")) {
            (Some(rest), Some(home)) => Path::new(&home).join(rest).to_string_lossy().into_owned(),
            _ => name.to_string(),
        };
        let mut out = String::new();
        let mut rest = name.replace('/', "\\");
        while let Some(a) = rest.find('%') {
            let Some(len) = rest[a + 1..].find('%') else { break };
            let var = &rest[a + 1..a + 1 + len];
            match std::env::var(var).ok().filter(|_| !var.is_empty()) {
                Some(v) => {
                    out.push_str(&rest[..a]);
                    out.push_str(&v);
                }
                None => out.push_str(&rest[..a + 1 + len + 1]),
            }
            rest = rest[a + 1 + len + 1..].to_string();
        }
        out + &rest
    } else {
        let name = name.replace('\\', "/");
        match (name.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => Path::new(&home).join(rest).to_string_lossy().into_owned(),
            _ => name,
        }
    }
}

/// `a/./b/../c` as `a/c` (by the names alone: links aren't followed).
fn tidy(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::Normal(_))) || !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_around_the_caret() {
        let line = r#"<link href=".\CSS\normalize.css"> see https://example.com/a?b=1. Done"#;
        assert_eq!(around(line, 16), r".\CSS\normalize.css");
        assert_eq!(around(line, 45), "https://example.com/a?b=1");
        assert_eq!(around("notes.txt:12", 3), "notes.txt:12");
        assert_eq!(around("a  b", 2), "");
        assert_eq!(around("", 0), "");
        assert_eq!(around("café.txt", 4), "café.txt");
    }

    #[test]
    fn what_looks_like_a_name() {
        for yes in ["notes.txt", r".\CSS\a.css", "src/main.rs:40", "https://x.org", "www.x.org", "Makefile/x", "a.md"] {
            assert!(plausible(yes), "{yes}");
        }
        for no in ["gamma", "1.5", "e.g", "v2.3", "", "end.", "U.S.A", "end.Then", "bob@example.com"] {
            assert!(!plausible(no), "{no}");
        }
        for yes in ["main.rs(40,5)", "a/bob@x.txt", "README.md", "file:///tmp/x"] {
            assert!(plausible(yes), "{yes}");
        }
    }

    #[test]
    fn links_quotes_and_compilers() {
        // a link keeps its brackets, wherever the place is in it
        let line = "see https://en.wikipedia.org/wiki/Slate_(disambiguation). And (https://x.org/a) too";
        assert_eq!(around(line, 10), "https://en.wikipedia.org/wiki/Slate_(disambiguation)");
        assert_eq!(around(line, 45), "https://en.wikipedia.org/wiki/Slate_(disambiguation)");
        assert_eq!(around(line, 70), "https://x.org/a");
        // curly quotes around a link aren't part of it; a host in brackets is; no host, no link
        assert_eq!(around("see “https://example.com/a” now", 10), "https://example.com/a");
        assert_eq!(around("at https://[::1]:8080/x here", 8), "https://[::1]:8080/x");
        assert!(target_of("https://", None).is_err());
        assert_eq!(target_of("https://[::1]:8080/x", None), Ok(Target::Link("https://[::1]:8080/x".into())));
        // a quoted name with a space in it, the place inside
        let q = r#"<a href="sub folder\with space.txt">"#;
        assert_eq!(around(q, 22), r"sub folder\with space.txt");
        // a colon after a name; a compiler's place
        assert_eq!(around("in notes.txt: see", 5), "notes.txt");
        let folder = std::env::temp_dir();
        assert_eq!(target_of("main.rs(40,5)", Some(&folder)), Ok(Target::File(folder.join("main.rs"), Some((40, 5)))));
        assert_eq!(target_of("main.rs(7)", Some(&folder)), Ok(Target::File(folder.join("main.rs"), Some((7, 1)))));
    }

    #[test]
    fn the_menu_says_where_it_goes() {
        let folder = std::env::temp_dir();
        let inside = Target::File(folder.join("a.txt"), None);
        assert_eq!(menu_label(&inside, Some(&folder), false), "Open “a.txt”");
        let elsewhere = Target::File(Path::new("/srv/other/b.txt").into(), None);
        assert!(menu_label(&elsewhere, Some(&folder), false).contains("other"));
        assert_eq!(menu_label(&Target::Link("https://example.com/x?y".into()), None, true), "&Open link “example.com”");
    }

    #[test]
    fn the_texts_menu_offers_names_only() {
        // a word selected (F7 selects the one spelled wrong) isn't offered as a file; File → Open selected file or
        // link still takes it
        let mut doc = Document::from_text(b"see brwn and notes.txt here");
        doc.path = Some(std::env::temp_dir().join("x.txt"));
        let word = Sel::new(4, 8);
        assert!(target_in(&doc, word, Some(5), true).is_err());
        assert!(matches!(target_in(&doc, word, Some(5), false), Ok(Target::File(..))));
        let name = Sel::new(13, 22);
        assert!(matches!(target_in(&doc, name, Some(15), true), Ok(Target::File(..))));
        // nothing selected: the name around the place, if it looks like one
        assert!(target_in(&doc, Sel::at(0), Some(5), true).is_err());
        assert!(matches!(target_in(&doc, Sel::at(0), Some(16), true), Ok(Target::File(..))));
    }

    #[test]
    fn links() {
        assert_eq!(target_of("https://example.com/x", None), Ok(Target::Link("https://example.com/x".into())));
        assert_eq!(target_of("  <HTTP://example.com>  ", None), Ok(Target::Link("HTTP://example.com".into())));
        assert_eq!(target_of("www.example.com", None), Ok(Target::Link("https://www.example.com".into())));
        assert!(target_of("javascript:alert(1)", None).is_err());
        assert!(target_of("ms-settings:display", None).is_err());
        assert!(target_of("https://a b", None).is_err());
        assert!(target_of("", None).is_err());
        assert!(target_of("one\ntwo", None).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn files_on_windows() {
        let folder = Path::new(r"C:\site\pages");
        let f = |t: &str| target_of(t, Some(folder));
        assert_eq!(f(r".\CSS\normalize.css"), Ok(Target::File(r"C:\site\pages\CSS\normalize.css".into(), None)));
        assert_eq!(f("../css/a.css"), Ok(Target::File(r"C:\site\css\a.css".into(), None)));
        assert_eq!(f(r#""D:\notes\todo.txt""#), Ok(Target::File(r"D:\notes\todo.txt".into(), None)));
        assert_eq!(f(r"D:\notes\todo.txt:12:5"), Ok(Target::File(r"D:\notes\todo.txt".into(), Some((12, 5)))));
        assert_eq!(f("main.rs:40"), Ok(Target::File(r"C:\site\pages\main.rs".into(), Some((40, 1)))));
        // (a drive's colon is no line number; a name with a number after its colon is)
        assert!(!matches!(f("C:5"), Ok(Target::File(_, Some(_)))));
        assert_eq!(f("readme:7"), Ok(Target::File(r"C:\site\pages\readme".into(), Some((7, 1)))));
        assert!(f("mailto:someone").is_err());
        assert!(target_of("a.txt", None).is_err());
        // `file:` addresses, and the home folder
        assert_eq!(target_of("file:///C:/notes/a%20b.txt", None), Ok(Target::File(r"C:\notes\a b.txt".into(), None)));
        let profile = std::env::var("USERPROFILE").unwrap();
        assert_eq!(f(r"~\notes.txt"), Ok(Target::File(Path::new(&profile).join("notes.txt"), None)));
        // an environment variable that's set, and one that isn't
        let home = std::env::var("SystemRoot").unwrap();
        assert_eq!(f(r"%SystemRoot%\win.ini"), Ok(Target::File(Path::new(&home).join("win.ini"), None)));
        assert_eq!(f(r"%NoSuchSlateVar%\x"), Ok(Target::File(r"C:\site\pages\%NoSuchSlateVar%\x".into(), None)));
    }

    #[cfg(not(windows))]
    #[test]
    fn files_elsewhere() {
        let folder = Path::new("/srv/site");
        let f = |t: &str| target_of(t, Some(folder));
        assert_eq!(f(r".\CSS\normalize.css"), Ok(Target::File("/srv/site/CSS/normalize.css".into(), None)));
        assert_eq!(f("../x/a.lua:3"), Ok(Target::File("/srv/x/a.lua".into(), Some((3, 1)))));
        assert_eq!(f("/etc/hosts"), Ok(Target::File("/etc/hosts".into(), None)));
        assert_eq!(target_of("file:///home/x/a%20b.txt", None), Ok(Target::File("/home/x/a b.txt".into(), None)));
        assert!(target_of("a.txt", None).is_err());
    }
}
