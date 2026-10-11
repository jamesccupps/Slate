//! The tabs and their unsaved text, kept between runs (like Windows 11 Notepad): `session.json` in the data folder
//! lists the tabs; a tab with unsaved changes (up to `KEEP_MAX`) has its text in a copy of its own next to it. The
//! rules are Windows' (`src/ui/session.rs`): every file is flushed to disk before it replaces the one before (and
//! session.json keeps the one before as session.json.bak); reading is lenient (a tab or a value this version can't
//! read loses nothing else); Slate deletes only the copies it wrote or read itself, and copies no tab refers to come
//! back as tabs of their own. While editing it's written on another thread; closing writes it in place.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::buffer::Snapshot;
use crate::core::document::Sel;
use crate::core::job::{Job, Notify};
use crate::core::text::{Encoding, Eol};
use crate::highlight::Lang;
use crate::settings;

use super::app::{App, KEEP_MAX};

#[derive(Serialize, Default)]
struct Session {
    tabs: Vec<SessionTab>,
    active: usize,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct SessionTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    /// The path of a file whose name isn't UTF-8 (JSON only holds text): its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path_bytes: Option<Vec<u8>>,
    /// The copy holding its unsaved text (in the session folder).
    #[serde(default)]
    backup: Option<String>,
    #[serde(default)]
    caret: u64,
    #[serde(default)]
    anchor: u64,
    #[serde(default)]
    top: u64,
    #[serde(default, deserialize_with = "lenient")]
    lang: Option<Lang>,
    #[serde(default)]
    lang_picked: bool,
    #[serde(default, deserialize_with = "lenient")]
    encoding: Option<Encoding>,
    #[serde(default)]
    bom: bool,
    #[serde(default, deserialize_with = "lenient")]
    eol: Option<Eol>,
}

impl SessionTab {
    fn set_path(&mut self, p: Option<&Path>) {
        (self.path, self.path_bytes) = match p {
            Some(p) if p.to_str().is_some() => (Some(p.to_path_buf()), None),
            Some(p) => (None, Some(p.as_os_str().as_bytes().to_vec())),
            None => (None, None),
        };
    }

    fn file(&self) -> Option<PathBuf> {
        self.path.clone().or_else(|| self.path_bytes.clone().map(|b| PathBuf::from(std::ffi::OsString::from_vec(b))))
    }
}

/// A value this version can't read (a newer one wrote it, or it's damaged) is left out, rather than the tab.
fn lenient<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).ok())
}

/// session.json read leniently: the tabs that can be read (the copy of one that can't comes back on its own, see
/// `orphans`). None if it isn't JSON at all.
fn parse(bytes: &[u8]) -> Option<Session> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let tabs = match v.get("tabs") {
        Some(Value::Array(a)) => a.iter().filter_map(|t| serde_json::from_value(t.clone()).ok()).collect(),
        _ => Vec::new(),
    };
    let active = v.get("active").and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(Session { tabs, active })
}

fn dir() -> PathBuf {
    settings::data_dir().join("session")
}

/// The copies this Slate wrote or read: the only ones it deletes.
static OWNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn own(name: &str) {
    OWNED.lock().unwrap().insert(name.to_string());
}

/// A name for a tab's copy that no other tab uses, now or in an earlier run (process ids come round again).
fn new_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    format!("tab-{:x}-{:x}-{}.txt", t, std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// Whether `name` (from session.json) is one of Slate's copies, in the session folder: `tab-…-….txt`, as this
/// version and 0.8 name them. Anything else is never read, written or deleted.
fn valid_name(name: &str) -> bool {
    let core = name.strip_prefix("tab-").and_then(|n| n.strip_suffix(".txt"));
    core.is_some_and(|c| !c.is_empty() && c.len() < 64 && c.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
}

/// A copy that came back as zero bytes only: written just before a power cut, it never reached the disk.
fn is_damaged(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|&b| b == 0)
}

/// Reads a tab's copy (never more than a copy can hold).
fn read_copy(path: &Path) -> io::Result<Vec<u8>> {
    if fs::metadata(path)?.len() > KEEP_MAX + (1 << 20) {
        return Err(io::Error::other("too big for a copy"));
    }
    fs::read(path)
}

/// Moves a damaged copy into `damaged/`, so it's neither restored nor deleted.
fn set_aside(d: &Path, name: &str) {
    let _ = fs::create_dir_all(d.join("damaged"));
    let _ = fs::rename(d.join(name), d.join("damaged").join(name));
    OWNED.lock().unwrap().remove(name);
}

/// Creates (or empties) `path` for writing, readable by this user only: it holds their text.
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)
}

/// Writes `snap` to `path` through a temp file that's flushed first, so a crash or power cut leaves the old copy or
/// the new one, never half of one.
fn write_copy(path: &Path, snap: &Snapshot) -> bool {
    let tmp = path.with_extension("tmp");
    let r = (|| -> io::Result<()> {
        let mut w = BufWriter::with_capacity(1 << 20, create_private(&tmp)?);
        let mut err = None;
        snap.chunks(0, snap.len(), &mut |c| match w.write_all(c) {
            Ok(()) => true,
            Err(e) => {
                err = Some(e);
                false
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r.is_ok()
}

/// session.json, flushed to disk first; the one it replaces becomes session.json.bak.
fn write_session_file(d: &Path, json: &[u8]) -> bool {
    let tmp = d.join("session.json.tmp");
    let cur = d.join("session.json");
    let r = (|| -> io::Result<()> {
        let mut f = create_private(&tmp)?;
        f.write_all(json)?;
        f.sync_all()?;
        drop(f);
        let _ = fs::rename(&cur, d.join("session.json.bak"));
        fs::rename(&tmp, &cur)?;
        // (the new names on disk too)
        File::open(d)?.sync_all()
    })();
    r.is_ok()
}

/// Deletes this Slate's copies that the session doesn't refer to any more, and leftovers of interrupted writes.
fn prune(d: &Path, keep: &[String]) {
    let Ok(rd) = fs::read_dir(d) else { return };
    let mut owned = OWNED.lock().unwrap();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("tab-") || keep.contains(&name) {
            continue;
        }
        let stale_tmp = name.ends_with(".tmp")
            && e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > Duration::from_secs(3600)));
        if (owned.contains(&name) || stale_tmp) && fs::remove_file(e.path()).is_ok() {
            owned.remove(&name);
        }
    }
}

/// Whether closing can keep tab `i`'s unsaved text (else it's asked about).
pub fn keeps(app: &App, i: usize) -> bool {
    settings::persist() && app.settings.restore_session && !app.session_failed && app.tabs[i].doc.len() <= KEEP_MAX
}

/// Why closing can't keep tab `i`'s unsaved text, for the question about it.
pub fn unkept_reason(app: &App, i: usize) -> &'static str {
    if !settings::persist() || !app.settings.restore_session {
        "Your changes will be lost if you don't save them."
    } else if app.session_failed {
        "Slate couldn't keep unsaved changes for next time (its data folder can't be written: is the disk full?)."
    } else if app.tabs[i].doc.len() > KEEP_MAX {
        "Slate keeps unsaved changes for next time for documents up to 64 MB, and this one is bigger."
    } else {
        "Your changes will be lost if you don't save them."
    }
}

/// One write of the session: worked out on the UI thread, carried out by `write` (there, or on another thread).
pub struct Plan {
    dir: PathBuf,
    session: Session,
    /// Copies to (re)write: file name, the text, and the tab and document version it is.
    copies: Vec<(String, Snapshot, u64, u64)>,
    /// The copies the session refers to (never deleted).
    keep: Vec<String>,
}

/// What a write did: whether all of it was written, and which copies (tab, name, document version).
pub struct Outcome {
    ok: bool,
    written: Vec<(u64, String, u64)>,
}

impl crate::core::job::Failure for Outcome {
    fn failure(_: &str) -> Self {
        Outcome { ok: false, written: Vec::new() }
    }
}

fn plan(app: &mut App) -> Option<Plan> {
    if !settings::persist() {
        return None;
    }
    let d = dir();
    let restore = app.settings.restore_session;
    let mut session = Session::default();
    let mut copies = Vec::new();
    let mut keep = Vec::new();
    for (i, tab) in app.tabs.iter_mut().enumerate() {
        let mut st = SessionTab {
            caret: tab.view.sel.caret,
            anchor: tab.view.sel.anchor,
            top: tab.view.top,
            lang: Some(tab.lang),
            lang_picked: tab.lang_picked,
            encoding: Some(tab.doc.encoding),
            bom: tab.doc.bom,
            eol: Some(tab.doc.eol),
            ..Default::default()
        };
        st.set_path(tab.doc.path.as_deref());
        // (its file still being read: where the session had it)
        if let Some((anchor, caret, top)) = tab.restore_at {
            (st.anchor, st.caret, st.top) = (anchor, caret, top);
        }
        if tab.doc.is_dirty() && restore {
            if tab.doc.len() <= KEEP_MAX && tab.doc.is_ready() && !tab.loading() {
                let name = tab.backup.get_or_insert_with(new_name).clone();
                if tab.session_version != tab.doc.version || !d.join(&name).exists() {
                    own(&name);
                    copies.push((name.clone(), tab.doc.snapshot(), tab.id, tab.doc.version));
                }
                keep.push(name.clone());
                st.backup = Some(name);
            } else if let Some(name) = tab.backup.clone() {
                // Can't be written now (too big now, its file being read): the copy written last stays the tab's;
                // after a crash, that's better than nothing (closing asks about it).
                keep.push(name.clone());
                st.backup = Some(name);
            }
        }
        if st.backup.is_none() && st.file().is_none() {
            // an empty untitled tab, or one the session doesn't keep: nothing to remember
            continue;
        }
        if i <= app.active {
            session.active = session.tabs.len();
        }
        session.tabs.push(st);
    }
    if !restore {
        session.tabs.clear();
    }
    Some(Plan { dir: d, session, copies, keep })
}

fn write(p: Plan) -> Outcome {
    let mut session = p.session;
    if fs::create_dir_all(&p.dir).is_err() {
        return Outcome { ok: false, written: Vec::new() };
    }
    let mut ok = true;
    let mut written = Vec::new();
    for (name, snap, id, version) in p.copies {
        // A copy the disk hasn't room for (and a little more) isn't written: filling the disk up again and again would
        // make other programs' writes fail too. (The one there stays; closing asks.)
        let need = snap.len();
        let room = !crate::core::io::free_space(&p.dir).is_some_and(|free| free < need + (need / 8).max(1 << 20));
        if room && write_copy(&p.dir.join(&name), &snap) {
            written.push((id, name, version));
        } else {
            ok = false;
            // The copy written before, if there is one, stays the tab's; with none, the session can't bring it back
            // (the next write tries again).
            if !p.dir.join(&name).exists() {
                for t in session.tabs.iter_mut().filter(|t| t.backup.as_deref() == Some(name.as_str())) {
                    t.backup = None;
                }
            }
        }
    }
    ok &= serde_json::to_vec_pretty(&session).is_ok_and(|json| write_session_file(&p.dir, &json));
    // (Otherwise every old copy stays: the session.json on disk may still need them.)
    if ok {
        prune(&p.dir, &p.keep);
    }
    Outcome { ok, written }
}

/// Notes which copies are on disk now; a write that didn't do all of it is tried again.
fn apply(app: &mut App, out: &Outcome) {
    for (id, name, version) in &out.written {
        if let Some(t) = app.tabs.iter_mut().find(|t| t.id == *id && t.backup.as_deref() == Some(name.as_str())) {
            t.session_version = *version;
        }
    }
    if !out.ok {
        app.session_dirty = true;
    }
}

/// Writes the session now (closing, logging out). Returns whether all of it was written.
pub fn save(app: &mut App) -> bool {
    // (one write at a time: a background one finishes first)
    if let Some(mut job) = app.session_job.take() {
        if let Some(out) = job.wait() {
            apply(app, &out);
        }
    }
    let Some(p) = plan(app) else { return true };
    app.session_dirty = false;
    let out = write(p);
    apply(app, &out);
    out.ok
}

/// Starts writing the session on another thread (`finish` takes the outcome), unless a write is still going on.
pub fn start(app: &mut App, notify: Notify) {
    if app.session_job.is_some() {
        return;
    }
    let Some(p) = plan(app) else { return };
    app.session_dirty = false;
    app.session_job = Some(Job::spawn(0, notify, move |_| write(p)));
}

/// Takes the outcome of `start`'s write, if it's done. Returns whether one is still going on.
pub fn poll(app: &mut App) -> bool {
    let Some(job) = app.session_job.as_mut() else { return false };
    let Some(out) = job.take() else { return true };
    app.session_job = None;
    apply(app, &out);
    false
}

/// Loads session.json; if it's damaged, the one before it (session.json.bak). A damaged one is kept for a look.
fn load(d: &Path) -> Session {
    if let Ok(bytes) = fs::read(d.join("session.json")) {
        match parse(&bytes) {
            Some(s) => return s,
            None => {
                let _ = fs::rename(d.join("session.json"), d.join("session.json.bad"));
            }
        }
    }
    fs::read(d.join("session.json.bak")).ok().and_then(|b| parse(&b)).unwrap_or_default()
}

/// The copies no restored tab refers to (`claimed`), oldest first, as (file name, text); this Slate owns them from
/// now on. Empty ones aren't returned (they're deleted with the next write).
fn orphans(d: &Path, claimed: &[String]) -> Vec<(String, Vec<u8>)> {
    let Ok(rd) = fs::read_dir(d) else { return Vec::new() };
    let mut names: Vec<String> =
        rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| valid_name(n) && !claimed.contains(n)).collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let Ok(bytes) = read_copy(&d.join(&name)) else { continue };
        if is_damaged(&bytes) {
            set_aside(d, &name);
            continue;
        }
        own(&name);
        if !bytes.is_empty() {
            out.push((name, bytes));
        }
    }
    out
}

/// The last start stopped while it was putting the tabs back (Slate crashed): the session goes into
/// `crashed-<time>/`, so this start doesn't stop the same way. Returns that folder.
fn put_aside(d: &Path) -> PathBuf {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_secs());
    let to = d.join(format!("crashed-{secs}"));
    let _ = fs::create_dir_all(&to);
    if let Ok(rd) = fs::read_dir(d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("tab-") || n.starts_with("session.json") {
                let _ = fs::rename(e.path(), to.join(&n));
            }
        }
    }
    to
}

/// Puts the tabs of the last run back (text tabs at once, files read on other threads), and the copies no tab refers
/// to as tabs of their own. `watch`: notes that it's doing so until the tabs are shown (`restored`), so a start that
/// crashes on the way doesn't happen again and again.
pub fn restore(app: &mut App, watch: bool) {
    if !app.settings.restore_session || !settings::persist() {
        return;
    }
    let d = dir();
    let marker = d.join("restoring");
    if watch && marker.exists() {
        let to = put_aside(&d);
        let _ = fs::remove_file(&marker);
        app.flash(format!("Slate stopped while putting your tabs back last time, so they were set aside in {}.", to.display()), true);
        return;
    }
    let s = load(&d);
    if watch && (!s.tabs.is_empty() || d.join("session.json").exists()) {
        let _ = fs::create_dir_all(&d).and_then(|_| create_private(&marker));
    }
    let mut claimed = Vec::new();
    let mut missing = Vec::new();
    for st in &s.tabs {
        let path = st.file();
        let name = st.backup.as_ref().filter(|n| valid_name(n));
        if let Some(name) = name {
            claimed.push(name.clone());
            let text = match read_copy(&d.join(name)) {
                Ok(t) if is_damaged(&t) => {
                    set_aside(&d, name);
                    None
                }
                Ok(t) => Some(t),
                // (not read: it stays in the folder, not this Slate's to delete)
                Err(_) => None,
            };
            if let Some(text) = text {
                own(name);
                let i = app.add_text_tab(&text, path.clone(), st.lang);
                let tab = &mut app.tabs[i];
                tab.backup = Some(name.clone());
                tab.lang_picked = st.lang_picked;
                if let Some(e) = st.encoding {
                    tab.doc.encoding = e;
                }
                tab.doc.bom = st.bom;
                if let Some(e) = st.eol {
                    tab.doc.eol = e;
                }
                // what's on disk, so a change by another program is noticed
                if let Some(p) = &path {
                    tab.doc.disk = crate::core::io::disk_info(p);
                }
                let len = tab.doc.len();
                tab.view.sel = Sel::new(st.anchor.min(len), st.caret.min(len));
                tab.view.top = st.top.min(len);
                continue;
            }
            if path.is_none() {
                missing.push("an untitled tab".to_string());
                continue;
            }
        }
        // its file, read on another thread (one that isn't there any more says so then)
        let Some(p) = path else { continue };
        app.open_paths(std::slice::from_ref(&p));
        let i = app.tabs.len() - 1;
        let tab = &mut app.tabs[i];
        if st.lang_picked {
            if let Some(l) = st.lang {
                tab.lang = l;
                tab.lang_picked = true;
            }
        }
        tab.restore_at = Some((st.anchor, st.caret, st.top));
        if name.is_some() {
            missing.push(format!("the unsaved changes to {}", p.display()));
        }
    }
    for (name, text) in orphans(&d, &claimed) {
        let i = app.add_text_tab(&text, None, None);
        app.tabs[i].backup = Some(name);
    }
    if !app.tabs.is_empty() {
        app.active = s.active.min(app.tabs.len() - 1);
    }
    if !missing.is_empty() {
        app.flash(format!("Couldn't bring back {}.", missing.join(", ")), true);
    }
}

/// The restored tabs are shown: the next start puts them back as usual (see `restore`).
pub fn restored() {
    if settings::persist() {
        let _ = fs::remove_file(dir().join("restoring"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_or_tab_this_version_cant_read_loses_nothing_else() {
        // a language from a newer version, a damaged tab, and an unknown field
        let s = parse(
            br#"{"tabs": [{"backup": "tab-1-2.txt", "lang": "Zig", "eol": "Lf", "caret": 4},
                          {"backup": 5},
                          {"path": "/a/b.txt", "new_thing": [1], "encoding": {"x": 1}}], "active": "x"}"#,
        )
        .unwrap();
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].backup.as_deref(), Some("tab-1-2.txt"));
        assert!(s.tabs[0].lang.is_none());
        assert_eq!(s.tabs[0].eol, Some(Eol::Lf));
        assert_eq!(s.tabs[0].caret, 4);
        assert_eq!(s.tabs[1].file(), Some(PathBuf::from("/a/b.txt")));
        assert!(s.tabs[1].encoding.is_none());
        assert_eq!(s.active, 0);
        assert!(parse(b"{\"tabs\": [").is_none());
    }

    #[test]
    fn only_slates_own_names_are_used() {
        assert!(valid_name("tab-1234-1.txt"));
        assert!(valid_name(&new_name()));
        for bad in ["../x.txt", "tab-/etc/passwd.txt", "tab-1.tmp", "tab-.txt", "/tmp/tab-1.txt", "tab-1-2.txt/..", "tab-zz.txt"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_ne!(new_name(), new_name());
    }

    #[test]
    fn a_name_that_isnt_utf8_is_kept() {
        let p = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/caf\xe9.txt".to_vec()));
        let mut st = SessionTab::default();
        st.set_path(Some(&p));
        let back: SessionTab = serde_json::from_slice(&serde_json::to_vec(&st).unwrap()).unwrap();
        assert_eq!(back.file(), Some(p));
    }

    #[test]
    fn copies_no_tab_refers_to_come_back_and_others_stay() {
        let d = std::env::temp_dir().join(format!("slate-session-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("tab-1-1.txt"), "kept").unwrap();
        fs::write(d.join("tab-1-2.txt"), "orphan").unwrap();
        fs::write(d.join("tab-1-3.txt"), [0u8; 16]).unwrap();
        fs::write(d.join("notes.txt"), "not ours").unwrap();
        let o = orphans(&d, &["tab-1-1.txt".to_string()]);
        assert_eq!(o, vec![("tab-1-2.txt".to_string(), b"orphan".to_vec())]);
        // the zeros were set aside, not lost; pruning deletes only what this Slate owns
        assert!(d.join("damaged/tab-1-3.txt").exists());
        prune(&d, &[]);
        assert!(!d.join("tab-1-2.txt").exists());
        assert!(d.join("tab-1-1.txt").exists() && d.join("notes.txt").exists());
        // a damaged session.json: the one before it
        fs::write(d.join("session.json"), "{\"tabs\": [").unwrap();
        fs::write(d.join("session.json.bak"), r#"{"tabs": [{"backup": "tab-1-1.txt"}]}"#).unwrap();
        assert_eq!(load(&d).tabs.len(), 1);
        assert!(d.join("session.json.bad").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
