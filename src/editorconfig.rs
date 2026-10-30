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
        let g = glob.strip_prefix('/').unwrap_or(glob);
        glob_match(g.as_bytes(), rel.as_bytes())
    } else {
        // (the name, in this folder or any below: `**/` before it)
        let name = rel.rsplit('/').next().unwrap_or(rel);
        glob_match(glob.as_bytes(), name.as_bytes())
    }
}

/// EditorConfig's globs: `*` (not `/`), `**` (anything), `?`, `[abc]`, `[!abc]`, `[a-z]`, `{a,b}`, `{1..9}`, `\` to
/// take the next character as it is.
fn glob_match(g: &[u8], s: &[u8]) -> bool {
    // braces first: each choice tried in turn
    if let Some((a, b)) = brace(g) {
        let inner = &g[a + 1..b];
        let (pre, post) = (&g[..a], &g[b + 1..]);
        if let Some((lo, hi)) = num_range(inner) {
            // {1..9}: a number in range, there
            let digits: usize = s.iter().skip(pre.len().min(s.len())).take_while(|c| c.is_ascii_digit() || **c == b'-').count();
            return (1..=digits).any(|n| {
                let at = s.len().min(pre.len());
                std::str::from_utf8(&s[at..at + n]).ok().and_then(|t| t.parse::<i64>().ok()).is_some_and(|v| v >= lo && v <= hi)
                    && glob_match(pre, &s[..at])
                    && glob_match(post, &s[at + n..])
            });
        }
        return split_choices(inner).iter().any(|c| {
            let mut whole = pre.to_vec();
            whole.extend_from_slice(c);
            whole.extend_from_slice(post);
            glob_match(&whole, s)
        });
    }
    wild(g, s)
}

/// The first `{…}` with a `,` or `..` in it (as positions of its braces).
fn brace(g: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < g.len() {
        match g[i] {
            b'\\' => i += 1,
            b'{' => {
                let mut depth = 0;
                let mut j = i;
                while j < g.len() {
                    match g[j] {
                        b'\\' => j += 1,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                let inner = &g[i + 1..j];
                                if inner.contains(&b',') || num_range(inner).is_some() {
                                    return Some((i, j));
                                }
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `{a,b,{c,d}}`'s choices (a nested brace stays whole for later).
fn split_choices(inner: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![Vec::new()];
    let mut depth = 0;
    let mut i = 0;
    while i < inner.len() {
        let c = inner[i];
        match c {
            b'\\' if i + 1 < inner.len() => {
                out.last_mut().unwrap().extend_from_slice(&inner[i..i + 2]);
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
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
fn num_range(inner: &[u8]) -> Option<(i64, i64)> {
    let s = std::str::from_utf8(inner).ok()?;
    let (a, b) = s.split_once("..")?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// `*`, `**`, `?`, `[…]` and `\` (no braces left).
fn wild(g: &[u8], s: &[u8]) -> bool {
    let (mut gi, mut si) = (0, 0);
    // where to go back to after a `*` (or `**`) that matched too little
    let mut back: Option<(usize, usize, bool)> = None;
    while si < s.len() {
        if gi < g.len() {
            match g[gi] {
                b'*' => {
                    let double = g.get(gi + 1) == Some(&b'*');
                    gi += if double { 2 } else { 1 };
                    back = Some((gi, si, double));
                    continue;
                }
                b'?' if s[si] != b'/' => {
                    gi += 1;
                    si += 1;
                    continue;
                }
                b'[' => {
                    if let Some((ok, len)) = class(&g[gi..], s[si]) {
                        if ok && s[si] != b'/' {
                            gi += len;
                            si += 1;
                            continue;
                        }
                    } else if s[si] == b'[' {
                        gi += 1;
                        si += 1;
                        continue;
                    }
                }
                b'\\' if gi + 1 < g.len() && g[gi + 1] == s[si] => {
                    gi += 2;
                    si += 1;
                    continue;
                }
                c if c != b'\\' && c == s[si] => {
                    gi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        // no match here: the last star takes one more character (a single one never takes `/`)
        match back {
            Some((bg, bs, double)) if double || s[bs] != b'/' => {
                back = Some((bg, bs + 1, double));
                gi = bg;
                si = bs + 1;
            }
            _ => return false,
        }
    }
    g[gi..].iter().all(|&c| c == b'*')
}

/// A `[…]` class at the start of `g`: whether `c` is in it, and its length (None: not a class, no `]`).
fn class(g: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(g.get(i), Some(b'!') | Some(b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < g.len() {
        let x = g[i];
        if x == b']' && !first {
            return Some((hit != negate, i + 1));
        }
        first = false;
        if g.get(i + 1) == Some(&b'-') && g.get(i + 2).is_some_and(|&y| y != b']') {
            if (x..=g[i + 2]).contains(&c) {
                hit = true;
            }
            i += 3;
            continue;
        }
        if x == c {
            hit = true;
        }
        i += 1;
    }
    None
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
