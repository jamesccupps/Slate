//! EditorConfig (`.editorconfig` files, editorconfig.org): what a project says about its files' indentation, line
//! endings, encoding and final newline. Both apps read it for a file as it's opened (and saved under a new name):
//! the `.editorconfig` files in its folder and the ones above, up to one with `root = true`; a nearer file wins over
//! one further up, and a later section over an earlier one.

use std::path::Path;

use crate::core::text::{Encoding, Eol};

/// What EditorConfig says about a file (None: nothing, or "unset").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Props {
    /// Indent with spaces (true) or tabs.
    pub spaces: Option<bool>,
    /// Columns of a level of indentation.
    pub indent_size: Option<u32>,
    pub eol: Option<Eol>,
    pub charset: Option<Encoding>,
    /// The file ends with a line break (true), or doesn't.
    pub final_newline: Option<bool>,
}

impl Props {
    pub fn is_empty(&self) -> bool {
        *self == Props::default()
    }
}

/// How many folders up `.editorconfig` files are looked for at most.
const DEPTH_MAX: usize = 32;
/// A `.editorconfig` bigger than this isn't one (it's skipped).
const FILE_MAX: u64 = 256 << 10;

/// What the `.editorconfig` files say about the file at `path` (absolute).
pub fn for_file(path: &Path) -> Props {
    let mut files = Vec::new();
    let mut dir = path.parent();
    while let Some(d) = dir {
        if files.len() >= DEPTH_MAX {
            break;
        }
        let f = d.join(".editorconfig");
        let text = std::fs::metadata(&f).ok().filter(|m| m.is_file() && m.len() <= FILE_MAX).and_then(|_| std::fs::read(&f).ok());
        if let Some(text) = text {
            let root = is_root(&text);
            files.push((d.to_path_buf(), text));
            if root {
                break;
            }
        }
        dir = d.parent();
    }
    // (the furthest first: nearer ones override it)
    let mut raw = Raw::default();
    for (d, text) in files.iter().rev() {
        let rel = path.strip_prefix(d).unwrap_or(path);
        apply_file(&mut raw, text, &rel.to_string_lossy().replace('\\', "/"));
    }
    raw.props()
}

/// The values as given (`indent_size = tab`, `tab_width`...), worked out into `Props` at the end.
#[derive(Default)]
struct Raw {
    style: Option<Option<bool>>,
    size: Option<Option<String>>,
    tab_width: Option<Option<u32>>,
    eol: Option<Option<Eol>>,
    charset: Option<Option<Encoding>>,
    final_newline: Option<Option<bool>>,
}

impl Raw {
    fn props(&self) -> Props {
        let tab_width = self.tab_width.flatten();
        let size = match self.size.clone().flatten().as_deref() {
            Some("tab") => tab_width,
            Some(n) => n.parse::<u32>().ok().filter(|&n| (1..=16).contains(&n)),
            None => None,
        };
        // (a tab width alone is the indent size too, as the spec says)
        let size = size.or(tab_width.filter(|_| self.size.is_none()));
        Props {
            spaces: self.style.flatten(),
            indent_size: size,
            eol: self.eol.flatten(),
            charset: self.charset.flatten(),
            final_newline: self.final_newline.flatten(),
        }
    }
}

/// `root = true` before the first section.
fn is_root(text: &[u8]) -> bool {
    for line in String::from_utf8_lossy(text).lines() {
        let l = line.trim();
        if l.starts_with('[') {
            return false;
        }
        if let Some((k, v)) = l.split_once('=') {
            if k.trim().eq_ignore_ascii_case("root") && v.trim().eq_ignore_ascii_case("true") {
                return true;
            }
        }
    }
    false
}

/// The sections of one file that match `rel` (the file's path from that file's folder, with `/`), in order.
fn apply_file(raw: &mut Raw, text: &[u8], rel: &str) {
    let mut on = false;
    for line in String::from_utf8_lossy(text).lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with(';') {
            continue;
        }
        if let Some(rest) = l.strip_prefix('[') {
            let Some(glob) = rest.strip_suffix(']') else { continue };
            on = section_matches(glob, rel);
            continue;
        }
        if !on {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
        let unset = v == "unset";
        match k.as_str() {
            "indent_style" => raw.style = Some(if unset { None } else { match v.as_str() { "space" => Some(true), "tab" => Some(false), _ => continue } }),
            "indent_size" => raw.size = Some((!unset).then_some(v)),
            "tab_width" => raw.tab_width = Some(if unset { None } else { v.parse::<u32>().ok().filter(|&n| (1..=16).contains(&n)) }),
            "end_of_line" => {
                raw.eol = Some(match v.as_str() {
                    "lf" => Some(Eol::Lf),
                    "crlf" => Some(Eol::Crlf),
                    // (CR alone: not a line ending Slate types)
                    _ => None,
                })
            }
            "charset" => {
                raw.charset = Some(match v.as_str() {
                    "utf-8" => Some(Encoding::Utf8),
                    "utf-8-bom" => Some(Encoding::Utf8Bom),
                    "utf-16le" => Some(Encoding::Utf16Le),
                    "utf-16be" => Some(Encoding::Utf16Be),
                    "latin1" => Some(Encoding::Ansi),
                    _ => None,
                })
            }
            "insert_final_newline" => raw.final_newline = Some(match v.as_str() { "true" => Some(true), "false" => Some(false), _ => None }),
            _ => {}
        }
    }
}

/// Whether a section's glob matches the file: a glob without `/` matches the name in any folder, one with `/`
/// the path from the `.editorconfig`'s folder (a leading `/` only anchors it there).
fn section_matches(glob: &str, rel: &str) -> bool {
    if glob.contains('/') {
        // (both from the folder's own `/`, as EditorConfig's C library has them: so `**/x` and `/**/` can stand for no
        // folder at all)
        let g = glob.strip_prefix('/').unwrap_or(glob);
        glob_match(&format!("/{g}"), &format!("/{rel}"))
    } else {
        // (the name, in this folder or any below: `**/` before it)
        let name = rel.rsplit('/').next().unwrap_or(rel);
        glob_match(glob, name)
    }
}

/// How deep braces may be nested in a glob (a deeper one matches nothing).
const BRACES_MAX: usize = 32;

/// EditorConfig's globs: `*` (not `/`), `**` (anything; `/**/` also no folder), `?`, `[abc]`, `[!abc]`, `[a-z]`,
/// `{a,b}`, `{1..9}` (a whole number in that range), `\` to take the next character as it is. Made into one regex, as
/// EditorConfig's own C library does: matched in time linear in the name however many braces there are (trying each
/// choice of each `{a,b}` in turn took twice as long for each: a header of 40 never finished, on the UI thread).
fn glob_match(glob: &str, s: &str) -> bool {
    let mut re = String::from("^");
    let mut ranges = Vec::new();
    let g: Vec<char> = glob.chars().collect();
    if !glob_to_regex(&g, &mut re, &mut ranges, 0) {
        return false;
    }
    re.push('$');
    let Ok(r) = regex::RegexBuilder::new(&re).size_limit(1 << 20).build() else { return false };
    let Some(c) = r.captures(s) else { return false };
    // (each `{n..m}` a group: its number in range)
    ranges.iter().enumerate().all(|(k, &(lo, hi))| {
        c.get(k + 1).and_then(|m| m.as_str().parse::<i64>().ok()).is_some_and(|v| v >= lo && v <= hi)
    })
}

/// Writes glob `g` as a regex into `re`, each `{n..m}` a group with its range in `ranges`. False when braces are nested
/// deeper than `BRACES_MAX`.
fn glob_to_regex(g: &[char], re: &mut String, ranges: &mut Vec<(i64, i64)>, depth: usize) -> bool {
    if depth > BRACES_MAX {
        return false;
    }
    let mut i = 0;
    while i < g.len() {
        match g[i] {
            '\\' if i + 1 < g.len() => {
                literal(re, g[i + 1]);
                i += 2;
                continue;
            }
            '/' if g[i..].starts_with(&['/', '*', '*', '/']) => {
                re.push_str("(?:/|/.*/)");
                i += 4;
                continue;
            }
            '*' if g.get(i + 1) == Some(&'*') => {
                re.push_str(".*");
                i += 2;
                continue;
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => match class(&g[i..], re) {
                Some(len) => {
                    i += len;
                    continue;
                }
                None => literal(re, '['),
            },
            '{' => {
                if let Some(close) = closing_brace(g, i) {
                    let inner = &g[i + 1..close];
                    if let Some((lo, hi)) = num_range(inner) {
                        re.push_str("([+-]?[0-9]+)");
                        ranges.push((lo, hi));
                        i = close + 1;
                        continue;
                    }
                    let choices = split_choices(inner);
                    if choices.len() > 1 {
                        re.push_str("(?:");
                        for (k, c) in choices.iter().enumerate() {
                            if k > 0 {
                                re.push('|');
                            }
                            if !glob_to_regex(c, re, ranges, depth + 1) {
                                return false;
                            }
                        }
                        re.push(')');
                        i = close + 1;
                        continue;
                    }
                }
                // (`{single}` and a `{` without its `}`: as they are)
                literal(re, '{');
            }
            c => literal(re, c),
        }
        i += 1;
    }
    true
}

fn literal(re: &mut String, c: char) {
    re.push_str(&regex::escape(c.encode_utf8(&mut [0; 4])));
}

/// The `}` closing the `{` at `g[open]`.
fn closing_brace(g: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut j = open;
    while j < g.len() {
        match g[j] {
            '\\' => j += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// `{a,b,{c,d}}`'s choices (a nested brace stays whole, made a regex of its own in turn).
fn split_choices(inner: &[char]) -> Vec<Vec<char>> {
    let mut out = vec![Vec::new()];
    let mut depth = 0;
    let mut i = 0;
    while i < inner.len() {
        let c = inner[i];
        match c {
            '\\' if i + 1 < inner.len() => {
                out.last_mut().unwrap().extend_from_slice(&inner[i..i + 2]);
                i += 2;
                continue;
            }
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(Vec::new());
                i += 1;
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(c);
        i += 1;
    }
    out
}

/// `{num1..num2}`'s numbers.
fn num_range(inner: &[char]) -> Option<(i64, i64)> {
    let s: String = inner.iter().collect();
    let (a, b) = s.split_once("..")?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// A `[…]` class at the start of `g`, written into `re` as one; its length in the glob (None: not a class, as with no
/// `]`, or with a `/` in it, which a class never matches: then the `[` is just a character).
fn class(g: &[char], re: &mut String) -> Option<usize> {
    let negate = matches!(g.get(1), Some('!') | Some('^'));
    let start = if negate { 2 } else { 1 };
    // (a `]` first in it is one of its characters)
    let mut end = start;
    loop {
        match g.get(end)? {
            ']' if end > start => break,
            '\\' => end += 2,
            _ => end += 1,
        }
    }
    let body = &g[start..end];
    if body.contains(&'/') {
        return None;
    }
    re.push('[');
    if negate {
        // (nor `/`, which only `**` matches)
        re.push_str("^/");
    }
    let mut k = 0;
    while k < body.len() {
        let c = body[k];
        if c == '\\' && k + 1 < body.len() {
            k += 1;
        } else if c == '-' && k > 0 && k + 1 < body.len() {
            // (a range, `a-z`)
            re.push('-');
            k += 1;
            continue;
        }
        if "\\[]^-&~".contains(body[k]) {
            re.push('\\');
        }
        re.push(body[k]);
        k += 1;
    }
    re.push(']');
    Some(end + 1)
}

impl crate::core::job::Failure for Props {
    fn failure(_: &str) -> Props {
        Props::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_as_editorconfig_says() {
        let m = |g: &str, s: &str| section_matches(g, s);
        assert!(m("*", "a.txt") && m("*", "src/a.txt"));
        assert!(m("*.py", "x/y/z.py") && !m("*.py", "z.pyc"));
        assert!(m("*.{js,ts}", "app.ts") && m("*.{js,ts}", "lib/app.js") && !m("*.{js,ts}", "app.json"));
        assert!(m("Makefile", "sub/Makefile") && !m("Makefile", "Makefile.am"));
        assert!(m("lib/**.js", "lib/a/b/c.js") && !m("lib/**.js", "src/c.js"));
        assert!(m("/lib/*.js", "lib/c.js") && !m("/lib/*.js", "lib/a/c.js"));
        assert!(m("*.[ch]", "x.c") && m("*.[ch]", "x.h") && !m("*.[ch]", "x.o") && m("*.[!o]", "x.c"));
        assert!(m("file{1..3}.txt", "file2.txt") && !m("file{1..3}.txt", "file4.txt"));
        assert!(m("{package.json,.travis.yml}", "package.json") && m("?.md", "a.md") && !m("?.md", "ab.md"));
        assert!(m("[[]x].txt", "[x].txt") || m("\\[x\\].txt", "[x].txt"));
        // `/**/` is also no folder; a range after a star; a class with `/` in it is just characters
        assert!(m("lib/**/*.js", "lib/a/b/c.js") && m("lib/**/*.js", "lib/c.js") && !m("lib/**/*.js", "src/lib/c.js"));
        assert!(m("**/x.c", "x.c") && m("**/x.c", "a/x.c"));
        assert!(m("*{1..3}.log", "app2.log") && !m("*{1..3}.log", "app4.log") && m("v{-2..2}", "v-1"));
        assert!(m("{a,{b,c}}.txt", "c.txt") && !m("{a,{b,c}}.txt", "d.txt") && m("{x}", "{x}"));
        assert!(!m("[a/]b", "ab") && m("a[!b]c", "axc") && !m("a[!b]c", "abc") && m("[a-c]", "b") && !m("[a-c]", "d"));
        // many braces: as quick as a few (each a choice tried in turn took twice as long for each)
        let t = std::time::Instant::now();
        let glob = "{a,b}".repeat(40);
        assert!(!m(&glob, &"a".repeat(39)) && m(&glob, &"ab".repeat(20)));
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        // braces nested deeper than that: matches nothing
        assert!(!m(&format!("{}x{}", "{a,".repeat(40), "}".repeat(40)), "x"));
    }

    #[test]
    fn nearer_files_and_later_sections_win() {
        let base = std::env::temp_dir().join(format!("slate-ec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sub = base.join("proj").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        // above the root: never read
        std::fs::write(base.join(".editorconfig"), "[*]\nindent_style = tab\ncharset = latin1\n").unwrap();
        std::fs::write(
            base.join("proj").join(".editorconfig"),
            "root = true\n\n[*]\nindent_style = space\nindent_size = 4\nend_of_line = lf\ninsert_final_newline = true\n\n[*.md]\nindent_size = 2\ninsert_final_newline = false\n\n[Makefile]\nindent_style = tab\n",
        )
        .unwrap();
        std::fs::write(sub.join(".editorconfig"), "[*.rs]\nindent_size = tab\ntab_width = 8\nend_of_line = crlf\ncharset = utf-8-bom\n").unwrap();
        let p = for_file(&sub.join("main.rs"));
        assert_eq!(p, Props { spaces: Some(true), indent_size: Some(8), eol: Some(Eol::Crlf), charset: Some(Encoding::Utf8Bom), final_newline: Some(true) });
        let p = for_file(&sub.join("notes.md"));
        assert_eq!((p.spaces, p.indent_size, p.final_newline, p.charset), (Some(true), Some(2), Some(false), None));
        let p = for_file(&base.join("proj").join("Makefile"));
        assert_eq!((p.spaces, p.indent_size), (Some(false), Some(4)));
        // "unset" takes a value back
        std::fs::write(sub.join(".editorconfig"), "[*.txt]\nend_of_line = unset\n").unwrap();
        assert_eq!(for_file(&sub.join("a.txt")).eol, None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
