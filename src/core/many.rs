//! Find in all tabs and Find in files (issue #15): the lines a search matches in several texts, gathered into one
//! list (a tab of results, each line of which goes back to where it was found). Both apps' jobs (`in_tabs`,
//! `in_files_results`) and Replace in all tabs' bookkeeping (`ReplaceInTabs`) are here, so the two apps say the same.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::buffer::Snapshot;
use super::job::Ctx;
use super::search::{Haystack, Matcher};
use crate::edit::{group, plural};

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
/// Folders Find in files passes over: version control, dependencies and build output (`bin` too, but only beside an
/// `obj`, as .NET makes them: alone it's often a folder of scripts).
const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "node_modules", "target", "obj", "__pycache__", ".venv", "venv", ".idea", ".vs"];
/// How much of a file says whether it's text: a zero byte in it (not UTF-16) makes it binary.
const HEAD: u64 = 8192;

/// The lines of `h` that matches of `m` start in, `max` at most (each once, however many matches it has), and how
/// many lines match in all (counted on past `max`). `ctx`'s Esc stops it; its progress is left to the caller.
pub fn lines_with(m: &Matcher, h: &dyn Haystack, max: usize, ctx: Option<&Ctx>) -> (Vec<Hit>, u64) {
    let len = h.hay_len();
    let mut out = Vec::new();
    // (the caller counts tabs or files, not this text's bytes: only Esc is passed on)
    let quiet = ctx.map(|c| Ctx { cancel: c.cancel.clone(), progress: Default::default() });
    // (the lines are counted from the start up to `pos`)
    let (mut pos, mut line, mut total) = (0u64, 0u64, 0u64);
    let mut last: Option<u64> = None;
    let mut scratch = Vec::new();
    m.each(h, 0, len, quiet.as_ref(), 8 << 20, &mut |hay, hs, s, _| {
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
/// further back, so the match shows), up to its end or `LINE_MAX` bytes; a cut never inside a character, and marked
/// with "…".
fn line_text(h: &dyn Haystack, at: u64, len: u64, scratch: &mut Vec<u8>) -> String {
    let a = at.saturating_sub(LINE_MAX as u64 / 3);
    let before = h.hay(a, at, scratch).to_vec();
    let (start, cut_front) = match before.iter().rposition(|&c| c == b'\n') {
        Some(i) => (a + i as u64 + 1, false),
        // (from a character's first byte)
        None => (a + before.iter().take_while(|&&c| c & 0xC0 == 0x80).count() as u64, a > 0),
    };
    let b = (start + LINE_MAX as u64).min(len);
    let bytes = h.hay(start, b, scratch);
    let (end, cut_end) = match bytes.iter().position(|&c| c == b'\n') {
        Some(e) => (e, false),
        None if b < len => (whole_chars(bytes), true),
        None => (bytes.len(), false),
    };
    let text = String::from_utf8_lossy(&bytes[..end]);
    let text = text.trim_end_matches('\r');
    format!("{}{text}{}", if cut_front { "…" } else { "" }, if cut_end { "…" } else { "" })
}

/// How much of `b` is whole characters: a character its end cuts short is left out.
fn whole_chars(b: &[u8]) -> usize {
    let mut i = b.len();
    while i > 0 && b.len() - i < 3 && b[i - 1] & 0xC0 == 0x80 {
        i -= 1;
    }
    let Some(&lead) = i.checked_sub(1).and_then(|k| b.get(k)) else { return b.len() };
    let need = match lead {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    };
    if b.len() - (i - 1) < need { i - 1 } else { b.len() }
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

/// Why Find in files ended before looking at everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// `FILES_MAX` files looked at.
    Files,
    /// `HITS_MAX` lines found.
    Hits,
}

/// What Find in files found under a folder.
#[derive(Default)]
pub struct InFiles {
    pub files: Vec<FileHits>,
    /// Files looked through.
    pub searched: usize,
    /// Files passed over: binary, over `FILE_SIZE_MAX`, unreadable, and online only (kept in the cloud: reading one
    /// would download it).
    pub binary: usize,
    pub big: usize,
    pub unreadable: usize,
    pub online: usize,
    /// Links to folders, not followed.
    pub folder_links: usize,
    pub cut: Option<Cut>,
    /// The folder it was in when it ended early (cut short, or Esc).
    pub stopped_in: Option<PathBuf>,
}

impl InFiles {
    pub fn skipped(&self) -> usize {
        self.binary + self.big + self.unreadable + self.online
    }
}

/// Find in files: the text files under `root` (folders in it too, but not `SKIP_DIRS` or hidden ones), in name order,
/// searched with `m`. A file over `FILE_SIZE_MAX`, binary (a zero byte in its first `HEAD` bytes, which are read
/// first), or online only is passed over. `ctx`'s progress counts the files looked at; Esc ends it with what it found.
pub fn in_files(m: &Matcher, root: &Path, ctx: &Ctx) -> InFiles {
    let mut out = InFiles::default();
    let mut hits = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if ctx.cancelled() {
            out.stopped_in = Some(dir);
            return out;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let dotnet = entries.iter().any(|e| e.file_name().eq_ignore_ascii_case("obj"));
        let mut subdirs = Vec::new();
        let mut files: Vec<(PathBuf, u64, bool)> = Vec::new();
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                // (a link to a file is read; one to a folder isn't followed: a loop, or a folder elsewhere entirely)
                match std::fs::metadata(e.path()) {
                    Ok(md) if md.is_file() => files.push((e.path(), md.len(), online_only(&md))),
                    Ok(md) if md.is_dir() => out.folder_links += 1,
                    _ => {}
                }
            } else if ft.is_dir() {
                let skip = name.starts_with('.')
                    || SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name))
                    || (dotnet && name.eq_ignore_ascii_case("bin"));
                if depth < DEPTH_MAX && !skip {
                    subdirs.push(e.path());
                }
            } else if ft.is_file() {
                // (from the folder's own listing: nothing opened, an online-only file not fetched)
                match e.metadata() {
                    Ok(md) => files.push((e.path(), md.len(), online_only(&md))),
                    Err(_) => out.unreadable += 1,
                }
            }
        }
        // (a folder's files first, then its folders, in name order)
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth + 1));
        }
        for (f, len, online) in files {
            if out.searched + out.skipped() >= FILES_MAX || hits >= HITS_MAX {
                out.cut = Some(if hits >= HITS_MAX { Cut::Hits } else { Cut::Files });
                out.stopped_in = Some(dir);
                return out;
            }
            if ctx.cancelled() {
                out.stopped_in = Some(dir);
                return out;
            }
            ctx.progress.fetch_add(1, Ordering::Relaxed);
            if online {
                out.online += 1;
                continue;
            }
            if len > FILE_SIZE_MAX {
                out.big += 1;
                continue;
            }
            let data = match read_text(&f) {
                Got::Text(d) => d,
                Got::Binary => {
                    out.binary += 1;
                    continue;
                }
                Got::Unreadable => {
                    out.unreadable += 1;
                    continue;
                }
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

/// A file kept in the cloud (OneDrive's "online only" and the like): reading it would download it.
#[cfg(windows)]
fn online_only(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    md.file_attributes() & (0x1000 | 0x4_0000 | 0x40_0000) != 0
}

#[cfg(not(windows))]
fn online_only(_: &std::fs::Metadata) -> bool {
    false
}

enum Got {
    Text(Vec<u8>),
    Binary,
    Unreadable,
}

/// A file as Find in files searches it: its first `HEAD` bytes say whether it's text (binary files aren't read
/// further), then the rest is read and made UTF-8 (`text_of`).
fn read_text(path: &Path) -> Got {
    let Ok(mut f) = std::fs::File::open(path) else { return Got::Unreadable };
    let mut data = Vec::new();
    if (&mut f).take(HEAD).read_to_end(&mut data).is_err() {
        return Got::Unreadable;
    }
    let wide = utf16_kind(&data);
    if wide.is_none() && data.contains(&0) {
        return Got::Binary;
    }
    if f.read_to_end(&mut data).is_err() {
        return Got::Unreadable;
    }
    Got::Text(text_of(data, wide))
}

/// Whether a file starting with `head` is UTF-16 (`Some(true)`: big endian): a byte order mark, or text that is
/// mostly ASCII with every other byte zero (and the others almost never zero or control characters).
fn utf16_kind(head: &[u8]) -> Option<bool> {
    match head {
        [0xFF, 0xFE, ..] => return Some(false),
        [0xFE, 0xFF, ..] => return Some(true),
        _ => {}
    }
    let pairs = head.as_chunks::<2>().0;
    if pairs.len() < 4 {
        return None;
    }
    let n = pairs.len();
    let looks = |hi: usize, lo: usize| {
        let zero_hi = pairs.iter().filter(|p| p[hi] == 0).count();
        let odd_lo = pairs.iter().filter(|p| p[lo] == 0 || (p[lo] < 0x20 && !matches!(p[lo], b'\t' | b'\n' | b'\r'))).count();
        zero_hi * 10 >= n * 4 && odd_lo * 20 <= n
    };
    if looks(1, 0) {
        Some(false)
    } else if looks(0, 1) {
        Some(true)
    } else {
        None
    }
}

/// A file's text as Find in files searches it, made UTF-8: UTF-16 (`wide`: big endian or not) decoded, a UTF-8 byte
/// order mark left out, and text that isn't UTF-8 read as ANSI (Windows-1252 and the like), as Slate opens it.
fn text_of(data: Vec<u8>, wide: Option<bool>) -> Vec<u8> {
    if let Some(big) = wide {
        let body = match data.as_slice() {
            [0xFF, 0xFE, rest @ ..] | [0xFE, 0xFF, rest @ ..] => rest,
            all => all,
        };
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|&c| if big { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) }).collect();
        return String::from_utf16_lossy(&units).into_bytes();
    }
    let body = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&data);
    if std::str::from_utf8(body).is_ok() { body.to_vec() } else { super::text::ansi_to_utf8(body) }
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
#[derive(Default)]
pub struct Results {
    pub text: String,
    pub rows: Vec<Option<Place>>,
    /// Lines found in all, and whether Esc stopped the search (what's listed is what it found until then).
    pub lines: u64,
    pub stopped: bool,
    /// Find in all tabs: each tab's text's version when it was looked through (a line number may have moved since).
    pub versions: HashMap<u64, u64>,
}

impl Results {
    /// The results as text: a first line saying what was looked for, then for each source its name and its lines, each
    /// with its number (from 1).
    pub fn build(head: String, groups: Vec<(String, Vec<(Place, String)>, u64)>) -> Results {
        let mut text = head;
        let mut rows = vec![None];
        let mut lines = 0;
        for (name, found, total) in groups {
            lines += total;
            text.push_str("\n\n");
            rows.push(None);
            let more = total.saturating_sub(found.len() as u64);
            text.push_str(&name);
            if more > 0 {
                text.push_str(&format!("  ({} more {} not listed)", group(more), if more == 1 { "line" } else { "lines" }));
            }
            rows.push(None);
            let width = found.iter().map(|(p, _)| line_of(p) + 1).max().unwrap_or(1).to_string().len();
            for (place, t) in found {
                text.push_str(&format!("\n  {:>width$}: {t}", line_of(&place) + 1));
                rows.push(Some(place));
            }
        }
        text.push('\n');
        Results { text, rows, lines, ..Default::default() }
    }

    /// Where line `n` (from 0) of the results goes.
    pub fn place(&self, n: u64) -> Option<&Place> {
        self.rows.get(n as usize).and_then(|p| p.as_ref())
    }

    /// Whether the line of `place` may have moved since the search: its tab's text is another version now
    /// (`version_now`).
    pub fn moved(&self, place: &Place, version_now: u64) -> bool {
        match place {
            Place::Tab(id, _) => self.versions.get(id).is_some_and(|&v| v != version_now),
            Place::File(..) => false,
        }
    }
}

fn line_of(p: &Place) -> u64 {
    match p {
        Place::Tab(_, l) | Place::File(_, l) => *l,
    }
}

/// A source of Find in all tabs: the tab's id, its text's version, the name shown, and its text.
pub type TabSource = (u64, u64, String, Snapshot);

/// Find in all tabs' job: the tabs looked through in turn (`ctx`'s progress counts them), `q` what was looked for.
/// Esc keeps what was found until then.
pub fn in_tabs(m: &Matcher, q: &str, srcs: Vec<TabSource>, ctx: &Ctx) -> Results {
    let n = srcs.len() as u64;
    let mut groups = Vec::new();
    let mut versions = HashMap::new();
    let mut left = HITS_MAX;
    for (id, version, name, snap) in srcs {
        if ctx.cancelled() {
            break;
        }
        versions.insert(id, version);
        let (hits, total) = lines_with(m, &snap, left, Some(ctx));
        left -= hits.len();
        if total > 0 {
            groups.push((name, hits.into_iter().map(|h| (Place::Tab(id, h.line), h.text)).collect(), total));
        }
        ctx.progress.fetch_add(1, Ordering::Relaxed);
    }
    let lines: u64 = groups.iter().map(|g: &(String, Vec<(Place, String)>, u64)| g.2).sum();
    let stopped = ctx.cancelled();
    let mut head = format!("“{q}” in {} of {}: {}", groups.len(), plural(n, "tab", "tabs"), plural(lines, "line", "lines"));
    if stopped {
        head.push_str("; stopped by you");
    }
    head.push_str(" (Enter or a double-click goes to one)");
    let mut r = Results::build(head, groups);
    r.stopped = stopped;
    r.versions = versions;
    r
}

/// Find in files' job: `in_files` under `root`, as a tab of results (names relative to `root`); its first line says
/// what was passed over and why it ended early, if it did.
pub fn in_files_results(m: &Matcher, q: &str, root: &Path, ctx: &Ctx) -> Results {
    let r = in_files(m, root, ctx);
    let stopped = ctx.cancelled();
    let lines: u64 = r.files.iter().map(|f| f.total).sum();
    let mut head = format!(
        "“{q}” in {}: {} in {} of {}",
        root.display(),
        plural(lines, "line", "lines"),
        r.files.len(),
        plural(r.searched as u64, "file", "files")
    );
    let over: Vec<String> = [(r.binary, "binary"), (r.big, "over 32 MB"), (r.online, "online only"), (r.unreadable, "unreadable")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{} {what}", group(*n as u64)))
        .collect();
    if !over.is_empty() {
        head.push_str(&format!("; passed over: {}", over.join(", ")));
    }
    if r.folder_links > 0 {
        head.push_str(&format!("; {} not followed", plural(r.folder_links as u64, "link to a folder", "links to folders")));
    }
    let at = match r.stopped_in.as_deref().map(|d| d.strip_prefix(root).unwrap_or(d)) {
        Some(d) if !d.as_os_str().is_empty() => d.display().to_string(),
        _ => "the top folder".to_string(),
    };
    match (r.cut, stopped) {
        (Some(Cut::Files), _) => head.push_str(&format!("; ended after {} files, in {at}", group(FILES_MAX as u64))),
        (Some(Cut::Hits), _) => head.push_str(&format!("; ended at {} lines found", group(HITS_MAX as u64))),
        (None, true) => head.push_str(&format!("; stopped by you, in {at}")),
        (None, false) => {}
    }
    head.push_str(" (Enter or a double-click opens one)");
    let groups = r
        .files
        .into_iter()
        .map(|f| {
            let name = f.path.strip_prefix(root).unwrap_or(&f.path).display().to_string();
            let lines = f.hits.into_iter().map(|h| (Place::File(f.path.clone(), h.line), h.text)).collect();
            (name, lines, f.total)
        })
        .collect();
    let mut res = Results::build(head, groups);
    res.stopped = stopped;
    res
}

/// Find in all tabs or Find in files while it runs: its job, the title its tab of results gets, what the status bar
/// says meanwhile (`status`), whether Esc stopped it, and the tab shown when it started with its text's version (its
/// results come to the front only if that's still so: not while someone types elsewhere).
pub struct Running {
    pub job: super::job::Job<Result<Results, String>>,
    pub title: String,
    /// "the files in Slate-dev", "every tab".
    pub what: String,
    pub files: bool,
    pub stopping: bool,
    pub from: (u64, u64),
}

impl Running {
    /// "Looking in the files in Slate-dev… 8,586 looked at (Esc stops)", "Looking in every tab… 2 of 5 (Esc stops)".
    pub fn status(&self) -> String {
        if self.stopping {
            return "Stopping…".to_string();
        }
        let done = self.job.done();
        let n = if self.files { format!("{} looked at", group(done)) } else { format!("{} of {}", group(done), group(self.job.total)) };
        format!("Looking in {}… {n} (Esc stops)", self.what)
    }

    /// Esc: the job stops, keeping what it found (true), unless it was stopping already.
    pub fn stop(&mut self) -> bool {
        if self.stopping {
            return false;
        }
        self.stopping = true;
        self.job.cancel();
        true
    }

    /// Whether its results come to the front: the tab shown and its text are as they were when it started.
    pub fn may_show(&self, shown: (u64, u64)) -> bool {
        shown == self.from
    }
}

/// Replace in all tabs (issue #15): the tabs whose Replace all still runs, what was replaced, and the tabs left as they
/// were, for the message at the end.
#[derive(Default)]
pub struct ReplaceInTabs {
    pub pending: Vec<u64>,
    /// What replaced the matches (as typed), the matches replaced and the tabs that had any.
    pub replacement: String,
    pub count: u64,
    pub tabs: u64,
    /// Tabs not replaced in: read only (results and matching lines too), still opening, busy with another task,
    /// closed meanwhile, or whose text changed while it worked (Replace all then changes nothing).
    pub read_only: u64,
    pub opening: u64,
    pub busy: u64,
    pub closed: u64,
    pub refused: u64,
}

impl ReplaceInTabs {
    /// Tab `id`'s Replace all ended: `count` matches replaced, or none (`applied` false) as its text changed meanwhile.
    pub fn finished(&mut self, id: u64, count: u64, applied: bool) {
        self.pending.retain(|&p| p != id);
        if !applied {
            self.refused += 1;
        } else if count > 0 {
            self.count += count;
            self.tabs += 1;
        }
    }

    /// Tabs gone (or whose task is: `running` says) count as done, as closed meanwhile.
    pub fn prune(&mut self, running: impl Fn(u64) -> bool) {
        let before = self.pending.len();
        self.pending.retain(|&id| running(id));
        self.closed += (before - self.pending.len()) as u64;
    }

    pub fn done(&self) -> bool {
        self.pending.is_empty()
    }

    /// What it did: "Replaced 3 matches with “x” in 2 tabs; 1 read-only tab not changed".
    pub fn message(&self) -> String {
        let mut msg = if self.count == 0 {
            "No matches to replace in any tab".to_string()
        } else if self.replacement.is_empty() {
            format!("Removed {} in {}", plural(self.count, "match", "matches"), plural(self.tabs, "tab", "tabs"))
        } else {
            let r: String = if self.replacement.chars().count() > 30 {
                self.replacement.chars().take(29).chain(['…']).collect()
            } else {
                self.replacement.clone()
            };
            format!("Replaced {} with “{r}” in {}", plural(self.count, "match", "matches"), plural(self.tabs, "tab", "tabs"))
        };
        let left: Vec<String> = [
            (self.read_only, "read-only tab", "read-only tabs"),
            (self.opening, "tab still opening", "tabs still opening"),
            (self.busy, "busy tab", "busy tabs"),
            (self.closed, "tab closed meanwhile", "tabs closed meanwhile"),
            (self.refused, "tab whose text changed meanwhile", "tabs whose text changed meanwhile"),
        ]
        .iter()
        .filter(|(n, ..)| *n > 0)
        .map(|&(n, one, many)| plural(n, one, many))
        .collect();
        if !left.is_empty() {
            msg.push_str(&format!("; {} not changed", left.join(", ")));
        }
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::super::search::Query;
    use super::*;

    fn matcher(t: &str) -> Matcher {
        Matcher::new(&Query { text: t.into(), ..Default::default() }).unwrap()
    }

    fn ctx() -> Ctx {
        Ctx { cancel: Default::default(), progress: Default::default() }
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
        // a long line is cut, at both ends, and marked
        let long = format!("{}needle{}", "x".repeat(1000), "y".repeat(1000));
        let (hits, _) = lines_with(&matcher("needle"), &Bytes(long.as_bytes()), 9, None);
        let t = &hits[0].text;
        assert!(t.starts_with('…') && t.ends_with('…') && t.contains("needle") && t.len() <= LINE_MAX + 6, "{t}");
        // never inside a character, at either end
        let wide = format!("{}needle{}", "日".repeat(400), "本".repeat(400));
        let (hits, _) = lines_with(&matcher("needle"), &Bytes(wide.as_bytes()), 9, None);
        let t = &hits[0].text;
        assert!(!t.contains('\u{FFFD}') && t.starts_with("…日") && t.ends_with("本…"), "{t}");
        // (the job's progress isn't the text's bytes)
        let c = ctx();
        lines_with(&matcher("needle"), &Bytes(long.as_bytes()), 9, Some(&c));
        assert_eq!(c.progress.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn whole_characters() {
        let s = "aé日😀".as_bytes();
        for cut in 0..=s.len() {
            let n = whole_chars(&s[..cut]);
            assert!(std::str::from_utf8(&s[..n]).is_ok() && cut - n < 4, "{cut} -> {n}");
        }
        assert_eq!(whole_chars(&s[..s.len() - 1]), 6);
        assert_eq!(whole_chars(s), s.len());
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
        assert_eq!(r.lines, 7);
        assert_eq!(r.place(3), Some(&Place::Tab(1, 0)));
        assert_eq!(r.place(4), Some(&Place::Tab(1, 11)));
        assert_eq!(r.place(5), None);
        assert_eq!(r.place(7), Some(&Place::File("b.txt".into(), 4)));
    }

    #[test]
    fn in_tabs_keeps_versions_and_what_esc_left() {
        let doc = |t: &str| crate::core::document::Document::from_text(t.as_bytes()).snapshot();
        let srcs = vec![(1, 5, "a".to_string(), doc("x\nneedle")), (2, 9, "b".to_string(), doc("needle"))];
        let r = in_tabs(&matcher("needle"), "needle", srcs, &ctx());
        assert!(r.text.starts_with("“needle” in 2 of 2 tabs: 2 lines (Enter"), "{}", r.text);
        assert!(!r.stopped && r.moved(&Place::Tab(1, 1), 6) && !r.moved(&Place::Tab(1, 1), 5));
        let c = ctx();
        c.cancel.store(true, Ordering::Relaxed);
        let r = in_tabs(&matcher("needle"), "needle", vec![(1, 1, "a".to_string(), doc("needle"))], &c);
        assert!(r.stopped && r.lines == 0 && r.text.contains("stopped by you"));
    }

    #[test]
    fn replace_in_tabs_says_what_it_left() {
        let mut b = ReplaceInTabs { pending: vec![1, 2, 3], replacement: "pin".into(), read_only: 1, ..Default::default() };
        b.finished(1, 2, true);
        b.finished(2, 0, false);
        assert!(!b.done());
        b.prune(|id| id != 3);
        assert!(b.done());
        assert_eq!(b.message(), "Replaced 2 matches with “pin” in 1 tab; 1 read-only tab, 1 tab closed meanwhile, 1 tab whose text changed meanwhile not changed");
        let none = ReplaceInTabs::default();
        assert_eq!(none.message(), "No matches to replace in any tab");
        let gone = ReplaceInTabs { count: 3, tabs: 2, ..Default::default() };
        assert_eq!(gone.message(), "Removed 3 matches in 2 tabs");
    }

    #[test]
    fn utf16_without_a_mark_and_ansi() {
        let le: Vec<u8> = "plain text here, then more".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = "plain text here, then more".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(utf16_kind(&le), Some(false));
        assert_eq!(utf16_kind(&be), Some(true));
        assert_eq!(text_of(le, Some(false)), b"plain text here, then more");
        // (a binary file's zeros aren't every other byte)
        assert_eq!(utf16_kind(b"\x7FELF\x02\x01\x01\0\0\0\0\0\0\0\0\0\x03\0>\0\x01\0\0\0"), None);
        assert_eq!(utf16_kind(b"text"), None);
        // ANSI: read as Slate reads it
        assert_eq!(text_of(b"caf\xE9".to_vec(), None), "café".as_bytes());
        assert_eq!(text_of(b"\xEF\xBB\xBFok".to_vec(), None), b"ok");
    }

    #[test]
    fn find_in_files() {
        let root = std::env::temp_dir().join(format!("slate-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["sub", ".git", "node_modules", "scripts/bin", "app/bin", "app/obj"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("a.txt"), "one needle\ntwo").unwrap();
        std::fs::write(root.join("sub").join("b.md"), "x\nneedle here\nneedle there").unwrap();
        std::fs::write(root.join(".git").join("c"), "needle").unwrap();
        std::fs::write(root.join("node_modules").join("d.js"), "needle").unwrap();
        std::fs::write(root.join("e.bin"), b"needle\0\0").unwrap();
        // (a byte order mark isn't part of the line; UTF-16 is read as text, not passed over as binary)
        std::fs::write(root.join("f.txt"), b"\xEF\xBB\xBFneedle first").unwrap();
        let wide: Vec<u8> = [0xFFu8, 0xFE].into_iter().chain("x\nneedle".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        std::fs::write(root.join("g.txt"), wide).unwrap();
        // ANSI: found by its accented word
        std::fs::write(root.join("h.txt"), b"caf\xE9 needle").unwrap();
        // `bin` alone is looked in; beside `obj` (.NET's output) it isn't
        std::fs::write(root.join("scripts/bin/run.sh"), "needle").unwrap();
        std::fs::write(root.join("app/bin/out.txt"), "needle").unwrap();
        let c = ctx();
        let r = in_files(&matcher("needle"), &root, &c);
        let found: Vec<(String, Vec<u64>)> = r
            .files
            .iter()
            .map(|f| (f.path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"), f.hits.iter().map(|h| h.line).collect()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("a.txt".to_string(), vec![0]),
                ("f.txt".to_string(), vec![0]),
                ("g.txt".to_string(), vec![1]),
                ("h.txt".to_string(), vec![0]),
                ("scripts/bin/run.sh".to_string(), vec![0]),
                ("sub/b.md".to_string(), vec![1, 2])
            ]
        );
        assert_eq!(r.files[1].hits[0].text, "needle first");
        assert_eq!(r.files[3].hits[0].text, "café needle");
        assert_eq!((r.searched, r.binary, r.skipped(), r.cut), (6, 1, 1, None));
        // (the progress counts the files looked at)
        assert_eq!(c.progress.load(Ordering::Relaxed), 7);
        let res = in_files_results(&matcher("needle"), "needle", &root, &ctx());
        let head = res.text.lines().next().unwrap();
        assert!(head.contains(": 7 lines in 6 of 6 files; passed over: 1 binary (Enter"), "{head}");
        // Esc: what it found until then, and where it was
        let c = ctx();
        c.cancel.store(true, Ordering::Relaxed);
        let res = in_files_results(&matcher("needle"), "needle", &root, &c);
        assert!(res.stopped && res.text.contains("stopped by you, in the top folder"), "{}", res.text);
        let _ = std::fs::remove_dir_all(&root);
    }
}
