//! Find in all tabs and Find in files (issue #15): the lines a search matches in several texts, gathered into one
//! list (a tab of results, each line of which goes back to where it was found).

use std::path::{Path, PathBuf};

use super::job::Ctx;
use super::search::{Haystack, Matcher};

/// A line a match starts in: its number (from 0) and its text (`LINE_MAX` bytes at most, without its line break).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub line: u64,
    pub text: String,
}

/// The most of a line a result shows.
pub const LINE_MAX: usize = 300;
/// Results kept in all (the rest are counted only).
pub const HITS_MAX: usize = 20_000;
/// Find in files: files looked at at most, the biggest one read, and how deep into folders it goes.
pub const FILES_MAX: usize = 20_000;
pub const FILE_SIZE_MAX: u64 = 32 << 20;
const DEPTH_MAX: usize = 32;
/// Folders Find in files passes over: version control, dependencies and build output.
const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules", "target", "bin", "obj", "__pycache__", ".venv", "venv", ".idea", ".vs"];

/// The lines of `h` that matches of `m` start in, `max` at most (each once, however many matches it has), and how
/// many lines match in all (counted on past `max`).
pub fn lines_with(m: &Matcher, h: &dyn Haystack, max: usize, ctx: Option<&Ctx>) -> (Vec<Hit>, u64) {
    let len = h.hay_len();
    let mut out = Vec::new();
    // (the lines are counted from the start up to `pos`)
    let (mut pos, mut line, mut total) = (0u64, 0u64, 0u64);
    let mut last: Option<u64> = None;
    let mut scratch = Vec::new();
    m.each(h, 0, len, ctx, 8 << 20, &mut |hay, hs, s, _| {
        if s > pos {
            let he = hs + hay.len() as u64;
            if pos >= hs && s <= he {
                line += newlines(&hay[(pos - hs) as usize..(s - hs) as usize]);
            } else {
                // (from before this window: a piece at a time)
                let mut a = pos;
                while a < s {
                    let b = (a + (1 << 20)).min(s);
                    line += newlines(h.hay(a, b, &mut scratch));
                    a = b;
                }
            }
            pos = s;
        }
        if last == Some(line) {
            return true;
        }
        last = Some(line);
        total += 1;
        if out.len() < max {
            out.push(Hit { line, text: line_text(h, s, len, &mut scratch) });
        }
        true
    });
    (out, total)
}

fn newlines(b: &[u8]) -> u64 {
    b.iter().filter(|&&c| c == b'\n').count() as u64
}

/// The line `at` is in, as a result shows it: from its start (or a third of `LINE_MAX` before `at`, if it starts
/// further back, so the match shows), up to its end or `LINE_MAX` bytes.
fn line_text(h: &dyn Haystack, at: u64, len: u64, scratch: &mut Vec<u8>) -> String {
    let a = at.saturating_sub(LINE_MAX as u64 / 3);
    let before = h.hay(a, at, scratch).to_vec();
    let start = match before.iter().rposition(|&c| c == b'\n') {
        Some(i) => a + i as u64 + 1,
        None => a,
    };
    let b = (start + LINE_MAX as u64).min(len);
    let bytes = h.hay(start, b, scratch);
    let end = bytes.iter().position(|&c| c == b'\n').unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let text = text.trim_end_matches('\r');
    if start > 0 && !before.contains(&b'\n') && a > 0 { format!("…{text}") } else { text.to_string() }
}

/// Bytes searched as a text (a file read whole for Find in files).
pub struct Bytes<'a>(pub &'a [u8]);

impl Haystack for Bytes<'_> {
    fn hay_len(&self) -> u64 {
        self.0.len() as u64
    }
    fn hay<'a>(&'a self, a: u64, b: u64, _: &'a mut Vec<u8>) -> &'a [u8] {
        &self.0[a as usize..b as usize]
    }
}

/// One file's results for Find in files.
pub struct FileHits {
    pub path: PathBuf,
    pub hits: Vec<Hit>,
    pub total: u64,
}

/// What Find in files found under a folder.
#[derive(Default)]
pub struct InFiles {
    pub files: Vec<FileHits>,
    /// Files looked through, and those passed over (too big, binary, unreadable).
    pub searched: usize,
    pub skipped: usize,
    /// Stopped at `FILES_MAX` files, or at `HITS_MAX` results.
    pub cut: bool,
}

/// Find in files: the text files under `root` (folders in it too, but not `SKIP_DIRS` or hidden ones), in name order,
/// searched with `m`. A file over `FILE_SIZE_MAX`, or with a zero byte in its first 8 KB (binary), is passed over.
pub fn in_files(m: &Matcher, root: &Path, ctx: &Ctx) -> InFiles {
    let mut out = InFiles::default();
    let mut hits = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut files = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        if ctx.cancelled() {
            return out;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            // (links aren't followed: a loop, or a folder somewhere else entirely)
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if depth < DEPTH_MAX && !name.starts_with('.') && !SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                    subdirs.push(e.path());
                }
            } else if ft.is_file() {
                files.push(e.path());
            }
        }
        // (a folder's files first, then its folders, in name order)
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth + 1));
        }
        for f in files.drain(..) {
            if out.searched + out.skipped >= FILES_MAX || hits >= HITS_MAX {
                out.cut = true;
                return out;
            }
            if ctx.cancelled() {
                return out;
            }
            let big = std::fs::metadata(&f).map_or(true, |md| md.len() > FILE_SIZE_MAX);
            let data = if big { None } else { std::fs::read(&f).ok() };
            let Some(data) = data.filter(|d| !d[..d.len().min(8192)].contains(&0)) else {
                out.skipped += 1;
                continue;
            };
            out.searched += 1;
            let (h, total) = lines_with(m, &Bytes(&data), HITS_MAX - hits, Some(ctx));
            if total > 0 {
                hits += h.len();
                out.files.push(FileHits { path: f, hits: h, total });
            }
        }
    }
    out
}

/// Where a line of a tab of results goes (`Results::rows`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A tab (its id) and a line in it (from 0).
    Tab(u64, u64),
    /// A file and a line in it (from 0).
    File(PathBuf, u64),
}

/// A tab of results: its text, and where each of its lines goes (None: a heading or a blank line).
pub struct Results {
    pub text: String,
    pub rows: Vec<Option<Place>>,
}

impl Results {
    /// The results as text: a first line saying what was looked for, then for each source its name and its lines, each
    /// with its number (from 1).
    pub fn build(head: String, groups: Vec<(String, Vec<(Place, String)>, u64)>) -> Results {
        let mut text = head;
        let mut rows = vec![None];
        for (name, lines, total) in groups {
            text.push_str("\n\n");
            rows.push(None);
            let more = total.saturating_sub(lines.len() as u64);
            text.push_str(&name);
            if more > 0 {
                text.push_str(&format!("  ({more} more lines not listed)"));
            }
            rows.push(None);
            let width = lines.iter().map(|(p, _)| line_of(p) + 1).max().unwrap_or(1).to_string().len();
            for (place, t) in lines {
                text.push_str(&format!("\n  {:>width$}: {t}", line_of(&place) + 1));
                rows.push(Some(place));
            }
        }
        text.push('\n');
        Results { text, rows }
    }

    /// Where line `n` (from 0) of the results goes.
    pub fn place(&self, n: u64) -> Option<&Place> {
        self.rows.get(n as usize).and_then(|p| p.as_ref())
    }
}

fn line_of(p: &Place) -> u64 {
    match p {
        Place::Tab(_, l) | Place::File(_, l) => *l,
    }
}

#[cfg(test)]
mod tests {
    use super::super::search::Query;
    use super::*;

    fn matcher(t: &str) -> Matcher {
        Matcher::new(&Query { text: t.into(), ..Default::default() }).unwrap()
    }

    #[test]
    fn the_lines_a_search_matches() {
        let text = b"alpha\nbeta gamma beta\n\ngamma\r\nbeta";
        let (hits, total) = lines_with(&matcher("beta"), &Bytes(text), 10, None);
        assert_eq!(total, 2);
        assert_eq!(hits, vec![Hit { line: 1, text: "beta gamma beta".into() }, Hit { line: 4, text: "beta".into() }]);
        let (hits, total) = lines_with(&matcher("gamma"), &Bytes(text), 1, None);
        assert_eq!((hits.len(), total, hits[0].line), (1, 2, 1));
        assert_eq!(lines_with(&matcher("gamma"), &Bytes(text), 9, None).0[1], Hit { line: 3, text: "gamma".into() });
        // a long line is cut
        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let (hits, _) = lines_with(&matcher("needle"), &Bytes(long.as_bytes()), 9, None);
        assert!(hits[0].text.starts_with('…') && hits[0].text.contains("needle") && hits[0].text.len() <= LINE_MAX + 4);
    }

    #[test]
    fn a_tab_of_results() {
        let r = Results::build(
            "“x” in 2 tabs".into(),
            vec![
                ("a.txt".into(), vec![(Place::Tab(1, 0), "x one".into()), (Place::Tab(1, 11), "x two".into())], 2),
                ("b.txt".into(), vec![(Place::File("b.txt".into(), 4), "x".into())], 5),
            ],
        );
        assert_eq!(r.text, "“x” in 2 tabs\n\na.txt\n   1: x one\n  12: x two\n\nb.txt  (4 more lines not listed)\n  5: x\n");
        assert_eq!(r.place(3), Some(&Place::Tab(1, 0)));
        assert_eq!(r.place(4), Some(&Place::Tab(1, 11)));
        assert_eq!(r.place(5), None);
        assert_eq!(r.place(7), Some(&Place::File("b.txt".into(), 4)));
    }

    #[test]
    fn find_in_files() {
        let root = std::env::temp_dir().join(format!("slate-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("a.txt"), "one needle\ntwo").unwrap();
        std::fs::write(root.join("sub").join("b.md"), "x\nneedle here\nneedle there").unwrap();
        std::fs::write(root.join(".git").join("c"), "needle").unwrap();
        std::fs::write(root.join("node_modules").join("d.js"), "needle").unwrap();
        std::fs::write(root.join("e.bin"), b"needle\0\0").unwrap();
        let ctx = Ctx { cancel: Default::default(), progress: Default::default() };
        let r = in_files(&matcher("needle"), &root, &ctx);
        let found: Vec<(String, Vec<u64>)> = r
            .files
            .iter()
            .map(|f| (f.path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), f.hits.iter().map(|h| h.line).collect()))
            .collect();
        assert_eq!(found, vec![("a.txt".to_string(), vec![0]), ("sub/b.md".to_string(), vec![1, 2])]);
        assert_eq!((r.searched, r.skipped, r.cut), (2, 1, false));
        let _ = std::fs::remove_dir_all(&root);
    }
}
