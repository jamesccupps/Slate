//! Remembers open tabs between runs, including unsaved text, like Windows 11 Notepad: closing Slate doesn't ask
//! about unsaved changes; they come back next time. Stored in the data folder (see settings.rs) under `session\`
//! (`session-admin\` for a Slate running as administrator, which runs apart from a normal one): session.json plus,
//! per unsaved tab, a backup copy of its text (up to `BACKUP_LIMIT`) or, for a bigger document, its pieces (see
//! "Big documents" below).
//!
//! This is the user's unsaved work, so:
//! - Files are flushed to disk before they replace the old ones (after a power cut a renamed file can otherwise come
//!   back empty), and the session.json before is kept as session.json.bak.
//! - Reading is lenient: a tab that can't be read, or a value written by a newer Slate, doesn't lose the others.
//! - Slate deletes only backups it wrote or read itself. Backups no tab refers to (session.json couldn't be read,
//!   or Slate stopped between writing a backup and session.json) come back as tabs.
//! - While editing, the session is written on another thread (backups of big documents take a moment); closing and
//!   shutting down write it right away.
//! - A tab that isn't back yet (a big document being put back, a file on a network share that doesn't answer) is
//!   written as it was read (`Restoring`), so nothing of it is lost meanwhile; so is one whose file is still being
//!   read (`Tab::place`).

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::core::buffer::{Buffer, Snapshot};
use crate::core::document::{DiskInfo, Document};
use crate::core::job::{Ctx, Failure, Job, Notify};
use crate::core::source::{Identity, Source, SourceKind, StableHasher, stable_hash};
use crate::core::text::{Encoding, Eol};

use super::app::Tab;
use super::highlight::Lang;
use super::settings::data_dir;

/// Unsaved documents up to this size are backed up as a copy; bigger ones as their pieces ("Big documents").
pub const BACKUP_LIMIT: u64 = 64 << 20;

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionTab {
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub untitled: u32,
    #[serde(default)]
    pub backup: Option<String>,
    /// A big document's pieces (`<name>.pieces` and `<name>.data`, see "Big documents").
    #[serde(default)]
    pub pieces: Option<String>,
    #[serde(default = "Fallback::fallback", deserialize_with = "lenient")]
    pub encoding: Encoding,
    #[serde(default = "yes")]
    pub bom: bool,
    #[serde(default = "Fallback::fallback", deserialize_with = "lenient")]
    pub eol: Eol,
    #[serde(default = "Fallback::fallback", deserialize_with = "lenient")]
    pub lang: Lang,
    #[serde(default)]
    pub lang_picked: bool,
    #[serde(default)]
    pub anchor: u64,
    #[serde(default)]
    pub caret: u64,
    #[serde(default)]
    pub top: u64,
    #[serde(default)]
    pub disk_len: Option<u64>,
    #[serde(default)]
    pub disk_modified_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    #[serde(default, deserialize_with = "each_tab")]
    pub tabs: Vec<SessionTab>,
    #[serde(default)]
    pub active: usize,
}

fn yes() -> bool {
    true
}

/// The value used for one this Slate doesn't know (a newer version wrote it).
trait Fallback {
    fn fallback() -> Self;
}

impl Fallback for Encoding {
    fn fallback() -> Self {
        Encoding::Utf8
    }
}

impl Fallback for Eol {
    fn fallback() -> Self {
        Eol::Crlf
    }
}

impl Fallback for Lang {
    fn fallback() -> Self {
        Lang::Plain
    }
}

fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Fallback>(d: D) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_else(|_| T::fallback()))
}

/// The tabs that can be read (the backup of one that can't comes back on its own, see `orphans`).
fn each_tab<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SessionTab>, D::Error> {
    let list = match serde_json::Value::deserialize(d)? {
        serde_json::Value::Array(a) => a,
        _ => Vec::new(),
    };
    Ok(list.into_iter().filter_map(|t| serde_json::from_value(t).ok()).collect())
}

/// Backup files this Slate wrote or read: the only ones it deletes.
static OWNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn own(name: &str) {
    OWNED.lock().unwrap().insert(name.to_string());
}

/// A backup file name no other tab uses, now or in an earlier run.
fn new_backup_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    format!("tab-{:x}-{:x}-{}.txt", t, std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

pub fn dir() -> PathBuf {
    data_dir().join(if super::win::elevated() { "session-admin" } else { "session" })
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn disk_from(t: &SessionTab) -> Option<DiskInfo> {
    Some(DiskInfo { len: t.disk_len?, modified: UNIX_EPOCH + Duration::from_millis(t.disk_modified_ms?) })
}

/// Whether a tab's text can be kept in the session.
pub fn can_back_up(tab: &Tab) -> bool {
    !tab.doc.is_dirty() || (tab.doc.is_ready() && (tab.doc.len() <= BACKUP_LIMIT || big_keepable(&tab.doc)))
}

/// One write of the session: worked out on the UI thread, carried out by `write` (there, or on another thread).
pub struct Plan {
    dir: PathBuf,
    session: Session,
    /// Backups to (re)write: file name, the text, and the tab and document version it is.
    backups: Vec<(String, Snapshot, u64, u64)>,
    /// Big documents to write (see "Big documents").
    big: Vec<BigWrite>,
    /// Every backup the session refers to.
    keep: Vec<String>,
    /// A big document waits to be written (after a write that failed): the session isn't all written yet.
    pending: bool,
}

pub struct Outcome {
    /// Everything was written.
    pub ok: bool,
    /// Something waits to be written later (see `Plan::pending`).
    pending: bool,
    /// (tab id, file name, document version) of each backup written.
    written: Vec<(u64, String, u64)>,
    /// Each big document's write.
    big: Vec<BigDone>,
}

/// How a big document's write went (`write_big`).
struct BigDone {
    tab: u64,
    name: String,
    /// It was written anew under `name`, in place of these files.
    replaces: Option<String>,
    version: u64,
    /// What was added to `<name>.data`.
    got: Result<Stored, BigErr>,
}

impl Failure for Outcome {
    fn failure(_: &str) -> Self {
        Outcome { ok: false, pending: false, written: Vec::new(), big: Vec::new() }
    }
}

/// Works out what to write. Backups are rewritten only for tabs that changed since the last write. `closing`: the
/// last write (big documents waiting after a failed write try once more).
fn plan(tabs: &mut [Tab], active: usize, closing: bool) -> Plan {
    plan_in(dir(), tabs, active, closing)
}

fn plan_in(d: PathBuf, tabs: &mut [Tab], active: usize, closing: bool) -> Plan {
    let mut backups = Vec::new();
    let mut big = Vec::new();
    let mut keep: Vec<String> = Vec::new();
    let mut list = Vec::new();
    let mut active_idx = 0;
    let mut pending = false;
    for (i, tab) in tabs.iter_mut().enumerate() {
        if tab.load_job.is_some() && tab.doc.path.is_none() {
            continue;
        }
        // Not back yet: as it was (its files are still needed). Its file still being read: as the session had it.
        if let Some(st) = tab.restore.as_ref().map(|r| &r.st).or(tab.place.as_ref()) {
            keep.extend(files_of(st));
            if i <= active {
                active_idx = list.len();
            }
            list.push(st.clone());
            continue;
        }
        let mut backup = None;
        let mut pieces = None;
        let mut stale = false;
        // (Not for a tab the user said "Don't save" to: it comes back as the file on disk, if any.)
        if tab.doc.is_dirty() && !tab.discard && tab.doc.is_ready() {
            if tab.doc.len() <= BACKUP_LIMIT {
                tab.big = None;
                let name = tab.backup_name.get_or_insert_with(new_backup_name).clone();
                if tab.backup_version != tab.doc.version || !d.join(&name).exists() {
                    own(&name);
                    backups.push((name.clone(), tab.doc.snapshot(), tab.id, tab.doc.version));
                }
                keep.push(name.clone());
                backup = Some(name);
            } else {
                let keepable = big_keepable(&tab.doc);
                if keepable && (closing || tab.big.as_ref().is_none_or(|b| !b.waiting())) {
                    pieces = Some(tab.big.get_or_insert_with(BigBackup::new).name.clone());
                } else {
                    // It waits after a write that failed (tried again then: the session isn't all written), or it
                    // can't be kept any more (closing asks). The list written last, if any, still holds the edits
                    // as they were then: after a crash, that's better than nothing.
                    pending |= keepable;
                    match tab.big.as_ref() {
                        Some(b) if b.listed => {
                            pieces = Some(b.name.clone());
                            stale = true;
                        }
                        // (waiting, nothing written yet: kept, with its tries, for the next one)
                        Some(_) if keepable => {}
                        _ => tab.big = None,
                    }
                }
            }
        } else {
            tab.big = None;
        }
        let doc = &tab.doc;
        if doc.path.is_none() && backup.is_none() && pieces.is_none() {
            // empty untitled tab: nothing to remember
            continue;
        }
        let mut st = SessionTab {
            path: doc.path.clone(),
            untitled: tab.untitled,
            backup,
            pieces: pieces.clone(),
            encoding: doc.encoding,
            bom: doc.bom,
            eol: doc.eol,
            lang: tab.lang,
            lang_picked: tab.lang_picked,
            anchor: tab.view.sel.anchor,
            caret: tab.view.sel.caret,
            top: tab.view.top,
            disk_len: doc.disk.map(|x| x.len),
            disk_modified_ms: doc.disk.map(|x| ms(x.modified)),
        };
        if let Some(name) = &pieces {
            let b = tab.big.as_ref().unwrap();
            // (also when a file the list names isn't there any more as it was: the text moved onto a saved file,
            // or another file took its place)
            let due =
                !stale && (b.version != tab.doc.version || b.outdated() || !d.join(pieces_file(name)).exists());
            match due.then(|| plan_big(b, &mut tab.doc, tab.id, &st)).flatten() {
                Some(w) => {
                    own(&pieces_file(&w.name));
                    own(&data_file(&w.name));
                    st.pieces = Some(w.name.clone());
                    big.push(w);
                    keep.extend(files_of(&st));
                }
                // (nothing new to write, or it can't be kept after all: the list written last stays)
                None if !due || b.listed => keep.extend(files_of(&st)),
                None => {
                    // (it reads from something that can't be kept after all, and no list was written: closing asks)
                    tab.big = None;
                    st.pieces = None;
                    if st.path.is_none() {
                        continue;
                    }
                }
            }
        }
        if i <= active {
            active_idx = list.len();
        }
        list.push(st);
    }
    Plan { dir: d, session: Session { tabs: list, active: active_idx }, backups, big, keep, pending }
}

/// The session's files of a tab.
fn files_of(st: &SessionTab) -> Vec<String> {
    let mut v: Vec<String> = st.backup.iter().cloned().collect();
    if let Some(n) = &st.pieces {
        v.push(pieces_file(n));
        v.push(data_file(n));
    }
    v
}

/// Writes the backups, then the session.json that refers to them, then deletes the backups no longer needed.
fn write(mut p: Plan) -> Outcome {
    if fs::create_dir_all(&p.dir).is_err() {
        return Outcome { ok: false, pending: p.pending, written: Vec::new(), big: Vec::new() };
    }
    let mut ok = true;
    let mut written = Vec::new();
    for (name, snap, id, version) in p.backups {
        // A copy the disk hasn't room for (and a little more) isn't written, as for big documents: filling the disk
        // up again and again would make other programs' writes fail too. (The one there stays; closing asks.) Asked
        // for each, so what was written before counts.
        let need = snap.len();
        let room = !crate::core::io::free_space(&p.dir).is_some_and(|free| free < need + (need / 8).max(1 << 20));
        if room && write_backup(&snap, &p.dir.join(&name)) {
            written.push((id, name, version));
        } else {
            ok = false;
        }
    }
    let mut big = Vec::new();
    for w in std::mem::take(&mut p.big) {
        let r = write_big(&p.dir, &w);
        if r.is_err() {
            ok = false;
            // A list that was never written (or whose added text is gone) can't be put back: the session refers to
            // the one before (it was being written anew: also if the new list got written, as it may not have all
            // its text), or to none for this tab (and the next write tries again).
            if w.replaces.is_some() || r == Err(BigErr::Lost) || !p.dir.join(pieces_file(&w.name)).exists() {
                let before = w.replaces.clone().filter(|n| p.dir.join(pieces_file(n)).exists());
                for t in p.session.tabs.iter_mut().filter(|t| t.pieces.as_deref() == Some(w.name.as_str())) {
                    t.pieces = before.clone();
                }
            }
        }
        let got = r.map(|(len, sums)| w.stored(len, sums));
        big.push(BigDone { tab: w.tab, name: w.name.clone(), replaces: w.replaces.clone(), version: w.version, got });
    }
    ok &= serde_json::to_vec_pretty(&p.session).is_ok_and(|json| write_session_file(&p.dir, &json));
    // (Otherwise every old backup stays: the session.json on disk may still need them.)
    if ok {
        prune(&p.dir, &p.keep);
    }
    Outcome { ok, pending: p.pending, written, big }
}

/// Notes which backups are on disk now.
fn apply(tabs: &mut [Tab], out: &Outcome) {
    for (id, name, version) in &out.written {
        if let Some(t) = tabs.iter_mut().find(|t| t.id == *id && t.backup_name.as_deref() == Some(name.as_str())) {
            t.backup_version = *version;
        }
    }
    for d in &out.big {
        if let Some(t) = tabs.iter_mut().find(|t| t.id == d.tab) {
            apply_big(&mut t.big, d);
        }
    }
}

/// What a big document's write (`d`) means for what the session holds of it (`big`).
fn apply_big(big: &mut Option<BigBackup>, d: &BigDone) {
    let was = d.replaces.as_ref().unwrap_or(&d.name);
    let Some(b) = big.as_mut().filter(|b| &b.name == was) else { return };
    match &d.got {
        // (written anew: those files from now on)
        Ok(s) if d.replaces.is_some() => {
            *b = BigBackup { name: d.name.clone(), stored: s.clone(), version: d.version, listed: true, retry: None };
        }
        Ok(s) => b.took(s, d.version),
        // `<name>.data` lost text its list refers to: new files next time.
        Err(BigErr::Lost) => *big = None,
        Err(BigErr::Failed { full }) => b.failed(*full),
    }
}

/// Writes the session now (closing, shutting down). Returns false if something couldn't be written.
pub fn save(tabs: &mut [Tab], active: usize) -> bool {
    if super::settings::guest() {
        return false;
    }
    if !super::settings::persist() {
        return true;
    }
    let out = write(plan(tabs, active, true));
    apply(tabs, &out);
    out.ok
}

/// Starts writing the session on another thread; `finish` takes the outcome.
pub fn start(tabs: &mut [Tab], active: usize, notify: Notify) -> Option<Job<Outcome>> {
    if !super::settings::persist() {
        return None;
    }
    let p = plan(tabs, active, false);
    Some(Job::spawn(0, notify, move |_| write(p)))
}

/// Takes the outcome of `start`'s write. Returns whether everything was written.
pub fn finish(tabs: &mut [Tab], out: Outcome) -> bool {
    apply(tabs, &out);
    // (a big document waiting to be written: not all of it, so it's tried again)
    out.ok && !out.pending
}

fn write_backup(snap: &Snapshot, path: &Path) -> bool {
    let tmp = path.with_extension("tmp");
    let r = (|| -> std::io::Result<()> {
        let mut w = std::io::BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?);
        let mut err = None;
        snap.chunks(0, snap.len(), &mut |c| {
            if let Err(e) = w.write_all(c) {
                err = Some(e);
                return false;
            }
            true
        });
        if let Some(e) = err {
            return Err(e);
        }
        // On the disk before it takes the name (a power cut could otherwise leave an empty backup under it).
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r.is_ok()
}

/// session.json, flushed to disk first; the one it replaces becomes session.json.bak.
fn write_session_file(dir: &Path, json: &[u8]) -> bool {
    let tmp = dir.join("session.json.tmp");
    let cur = dir.join("session.json");
    let r = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json)?;
        f.sync_all()?;
        drop(f);
        let _ = fs::rename(&cur, dir.join("session.json.bak"));
        fs::rename(&tmp, &cur)
    })();
    r.is_ok()
}

/// Deletes this Slate's backups that the session doesn't refer to any more, and leftovers of interrupted writes.
fn prune(dir: &Path, keep: &[String]) {
    let Ok(rd) = fs::read_dir(dir) else { return };
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

/// Reads the session; if session.json is damaged, the one before it (session.json.bak).
pub fn load() -> Option<Session> {
    let d = dir();
    if let Ok(bytes) = fs::read(d.join("session.json")) {
        match serde_json::from_slice(&bytes) {
            Ok(s) => return Some(s),
            // Kept for a look. The backups only it referred to come back as untitled tabs (`orphans`).
            Err(_) => {
                let _ = fs::rename(d.join("session.json"), d.join("session.json.bad"));
            }
        }
    }
    serde_json::from_slice(&fs::read(d.join("session.json.bak")).ok()?).ok()
}

pub fn read_backup(name: &str) -> Option<Vec<u8>> {
    if name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let bytes = fs::read(dir().join(name)).ok()?;
    own(name);
    Some(bytes)
}

/// A backup that came back as zero bytes only: it was written just before a power cut and never reached the disk.
pub fn is_damaged(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|&b| b == 0)
}

/// Moves a damaged backup into `damaged\`, so it's neither restored nor deleted.
pub fn set_aside(name: &str) {
    set_aside_in(&dir(), name);
}

fn set_aside_in(d: &Path, name: &str) {
    let to = d.join("damaged").join(name);
    let _ = fs::create_dir_all(d.join("damaged"));
    if fs::rename(d.join(name), &to).is_ok() {
        // (its time says when it was set aside: `prune_damaged` keeps it a while from then)
        let f = OpenOptions::new().access_mode(0x100).share_mode(0x7).open(&to); // FILE_WRITE_ATTRIBUTES
        let _ = f.and_then(|f| f.set_modified(SystemTime::now()));
    }
    OWNED.lock().unwrap().remove(name);
}

/// How long what was set aside in `damaged\` is kept.
const DAMAGED_DAYS: u64 = 30;

/// Deletes what was set aside in `damaged\` over `DAMAGED_DAYS` ago.
pub fn prune_damaged() {
    prune_damaged_in(&dir().join("damaged"), Duration::from_secs(DAMAGED_DAYS * 24 * 3600));
}

fn prune_damaged_in(dir: &Path, age: Duration) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > age));
        if old && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// The backups no restored tab refers to (`claimed`), oldest first, as (file name, text); this Slate owns them from
/// now on. Empty ones aren't returned (they're deleted with the next write). The added text of a big document
/// whose list of pieces is missing (Slate stopped before writing the first one) comes back like that too.
pub fn orphans(claimed: &[String]) -> Vec<(String, Vec<u8>)> {
    let d = dir();
    let Ok(rd) = fs::read_dir(&d) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("tab-") && !claimed.contains(n))
        .filter(|n| {
            n.ends_with(".txt")
                || n.strip_suffix(".data").is_some_and(|base| !d.join(pieces_file(base)).exists())
        })
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let path = d.join(&name);
        if fs::metadata(&path).map_or(true, |m| m.len() > BACKUP_LIMIT) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        if is_damaged(&bytes) {
            set_aside(&name);
            continue;
        }
        own(&name);
        if !bytes.is_empty() {
            out.push((name, bytes));
        }
    }
    out
}

/// Restoring the session crashed Slate: moves it (session.json and the backups) into `crashed-<time>\`, so the next
/// start doesn't fail the same way. Returns that folder.
pub fn put_aside() -> PathBuf {
    let d = dir();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_secs());
    let to = d.join(format!("crashed-{secs}"));
    let _ = fs::create_dir_all(&to);
    if let Ok(rd) = fs::read_dir(&d) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("tab-") || n.starts_with("session.json") {
                let _ = fs::rename(e.path(), to.join(&n));
            }
        }
    }
    OWNED.lock().unwrap().clear();
    to
}

/// Forgets the session (setting turned off): session.json and this Slate's backups. What it didn't write (another
/// Slate's backups, a session set aside after a crash, damaged backups) stays.
pub fn clear() {
    if !super::settings::persist() {
        return;
    }
    let d = dir();
    for name in ["session.json", "session.json.bak", "session.json.tmp"] {
        let _ = fs::remove_file(d.join(name));
    }
    let mut owned = OWNED.lock().unwrap();
    for name in std::mem::take(&mut *owned) {
        let _ = fs::remove_file(d.join(&name));
    }
}

// ---- Big documents ----
//
// A document over `BACKUP_LIMIT` isn't copied (that would mean writing hundreds of MB again and again). What the
// session keeps is its list of pieces, `<name>.pieces` (rewritten when the text changed; small), and in `<name>.data`
// what a later run couldn't read again otherwise (see `Keep`): typed and pasted text, each source once, and the parts
// the text uses of a self-deleting temp file or of a file that isn't at its path any more. `<name>.data` is only
// added to, and flushed to disk before the list that refers to it replaces the old one: a crash or power cut at any
// point leaves the last list and everything it refers to. Once most of it is what the text doesn't use any more
// (deleted, undone), it's written anew under another name with only what the text uses. The rest of the text is read
// from the user's files, which the list names with their `Identity`; putting the document back (`restore_big`) checks
// that they still hold the same bytes. If they don't (another program changed one, or it's gone), the edits are never
// laid over something else: the text that was added comes back on its own in a new tab, and the session's files are
// moved into `damaged\`.
//
// A document that would have to copy more than `COPY_LIMIT` from files (the result of Format or Replace all on a big
// file, a big file converted from UTF-16 or ANSI, a big file replaced by a save while undo still reads the old one)
// isn't kept: closing asks about it.

/// What the session holds of a big document.
pub struct BigBackup {
    /// Its files are `<name>.pieces` and `<name>.data`.
    pub name: String,
    /// What `<name>.data` holds (all of it on the disk).
    stored: Stored,
    /// The document version `<name>.pieces` holds (u64::MAX: none, or one to write again).
    version: u64,
    /// There is a `<name>.pieces`.
    listed: bool,
    /// A write failed (how many times in a row): when to try again (a full disk isn't tried every 20 s).
    retry: Option<(Instant, u32)>,
}

/// Hashes of parts of `<name>.data`: (from, to, `StableHasher` of what's there).
type Sums = Vec<(u64, u64, u64)>;

/// What `<name>.data` holds: its length in use, the parts of sources in it (source, from, to, where in the file),
/// and hashes of what was added at each write, checked when it's read back. And the files `<name>.pieces` names.
#[derive(Clone, Default)]
struct Stored {
    len: u64,
    parts: Vec<(Weak<Source>, u64, u64, u64)>,
    sums: Sums,
    named: Vec<Weak<Source>>,
}

/// Bytes of parts copied from files (`Keep::Parts`) a big document can have; beyond that, closing asks.
const COPY_LIMIT: u64 = BACKUP_LIMIT;
/// `<name>.data` is written anew once what the text doesn't use of it is more than this and more than what it uses.
const SLACK: u64 = 8 << 20;

impl BigBackup {
    fn new() -> BigBackup {
        let name = new_backup_name().trim_end_matches(".txt").to_string();
        BigBackup { name, stored: Stored::default(), version: u64::MAX, listed: false, retry: None }
    }

    /// The backup a document (`doc`) was just put back from: `data` over `<name>.data` (None if empty), and the
    /// `files` it read, as `list` says. Written anew soon if most of `<name>.data` isn't used.
    fn restored(
        name: &str,
        data: Option<&Arc<Source>>,
        files: &[Arc<Source>],
        list: &BigList,
        doc: &Document,
    ) -> BigBackup {
        let h = &list.header;
        let parts = data.iter().map(|d| (Arc::downgrade(d), 0, h.data_len, 0)).collect();
        let used: u64 = list.pieces.iter().filter(|p| p.0 == 0).map(|p| p.2).sum();
        let version = if h.data_len > used + used.max(SLACK) { u64::MAX } else { doc.version };
        let named = files.iter().map(Arc::downgrade).collect();
        let stored = Stored { len: h.data_len, parts, sums: h.sums.clone(), named };
        BigBackup { name: name.to_string(), stored, version, listed: true, retry: None }
    }

    /// Waiting after a write that failed.
    fn waiting(&self) -> bool {
        self.retry.is_some_and(|(at, _)| Instant::now() < at)
    }

    /// `<name>.pieces` names a file that isn't there any more as it was (`Source::is_gone`: a save put a new one in
    /// its place, or another program did) or that the text doesn't read any more (it moved onto a saved file): it
    /// has to be written again, also if the text didn't change.
    fn outdated(&self) -> bool {
        self.stored.named.iter().any(|w| w.upgrade().is_none_or(|s| s.is_gone()))
    }

    /// A write finished: what was added to `<name>.data` (`s`), for document `version`.
    fn took(&mut self, s: &Stored, version: u64) {
        self.stored.len = s.len;
        self.stored.parts.retain(|p| p.0.strong_count() > 0);
        self.stored.parts.extend(s.parts.iter().cloned());
        self.stored.sums = s.sums.clone();
        self.stored.named = s.named.clone();
        self.version = version;
        self.listed = true;
        self.retry = None;
    }

    /// A write failed (`full`: the disk is full): try again later, later each time.
    fn failed(&mut self, full: bool) {
        let n = self.retry.map_or(0, |r| r.1) + 1;
        let secs = if full { [60, 120, 300, 600] } else { [20, 60, 180, 600] }[(n as usize - 1).min(3)];
        self.retry = Some((Instant::now() + Duration::from_secs(secs), n));
    }
}

fn pieces_file(name: &str) -> String {
    format!("{name}.pieces")
}

fn data_file(name: &str) -> String {
    format!("{name}.data")
}

/// How the session keeps what one source of a big document holds.
#[derive(Clone)]
enum Keep {
    /// Read again from the file in a later run (the list names it).
    File(Identity),
    /// All of it copied into `<name>.data`: typed or pasted text, text kept from an earlier run.
    Whole,
    /// The parts the text uses copied into `<name>.data` (up to `COPY_LIMIT` in all): a self-deleting temp file, or
    /// a file that isn't at its path any more (`Source::is_gone`), which a later run couldn't read.
    Parts,
}

fn keep_of(src: &Source) -> Keep {
    match src.kind() {
        SourceKind::Memory | SourceKind::Session => Keep::Whole,
        SourceKind::File => src.identity().map_or(Keep::Parts, Keep::File),
        SourceKind::Temp => Keep::Parts,
    }
}

/// The bytes a big document's text has from sources kept as parts (`Keep::Parts`): (from self-deleting temp files,
/// from files that aren't at their path any more).
fn parts_bytes(doc: &Document) -> (u64, u64) {
    let buf = doc.buffer();
    let mut kinds: HashMap<u32, Option<bool>> = HashMap::new();
    let (mut temp, mut gone) = (0, 0);
    for p in buf.pieces() {
        let k = *kinds.entry(p.src).or_insert_with(|| {
            let s = buf.source(p.src);
            match s.kind() {
                SourceKind::Temp => Some(true),
                SourceKind::File if s.identity().is_none() => Some(false),
                _ => None,
            }
        });
        match k {
            Some(true) => temp += p.len,
            Some(false) => gone += p.len,
            None => {}
        }
    }
    (temp, gone)
}

/// Whether a big document can be kept as pieces (see `Keep`).
pub fn big_keepable(doc: &Document) -> bool {
    let (temp, gone) = parts_bytes(doc);
    temp + gone <= COPY_LIMIT
}

/// Why closing asks about tab `t` instead of keeping its changes (when the session can be written).
pub fn unkept_reason(t: &Tab) -> &'static str {
    if !t.doc.is_ready() {
        return "Slate is still reading this file's lines, so it can't keep these changes for next time yet.";
    }
    let (temp, gone) = parts_bytes(&t.doc);
    if gone > temp {
        "Slate can't keep these changes for next time: the file was replaced or moved while Slate still read from it, \
         and that's too much to copy."
    } else {
        "Slate can't keep these changes for next time: they come from Format, Replace all or a conversion of a big \
         file, which is too much to copy."
    }
}

/// One big document's part of a write.
struct BigWrite {
    tab: u64,
    name: String,
    /// Written anew (only what the text uses): the files it replaces.
    replaces: Option<String>,
    version: u64,
    /// `<name>.data`'s length in use before (what's after it is left from an interrupted write).
    data_from: u64,
    /// Parts of sources to add to `<name>.data`, one after the other from `data_from`: (source, from, to).
    add: Vec<(Arc<Source>, u64, u64)>,
    /// The hashes of what `<name>.data` holds before.
    sums: Sums,
    /// The new `<name>.pieces` (its `data_len` and `sums` are filled in once `<name>.data` is written), and the
    /// files it names.
    header: PiecesHeader,
    pieces: Pieces,
    named: Vec<Weak<Source>>,
}

impl BigWrite {
    /// What this added to `<name>.data` (now `len` long, with these hashes).
    fn stored(&self, len: u64, sums: Sums) -> Stored {
        let mut at = self.data_from;
        let mut parts = Vec::new();
        for (s, a, b) in &self.add {
            parts.push((Arc::downgrade(s), *a, *b, at));
            at += b - a;
        }
        Stored { len, parts, sums, named: self.named.clone() }
    }
}

/// A big document's write didn't happen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BigErr {
    /// `<name>.data` lost text its list refers to: start over with new files.
    Lost,
    /// Couldn't write now (`full`: the disk is full): try again later.
    Failed { full: bool },
}

/// The head of `<name>.pieces`.
#[derive(Serialize, Deserialize, Clone)]
pub struct PiecesHeader {
    /// The tab, for one that comes back without session.json (see `big_orphans`).
    #[serde(default)]
    pub tab: Option<SessionTab>,
    /// The document's length.
    pub len: u64,
    /// The bytes of `<name>.data` the pieces read.
    pub data_len: u64,
    /// The files pieces read from: source k is `files[k - 1]` (source 0 is `<name>.data`).
    #[serde(default)]
    pub files: Vec<Identity>,
    /// Hashes of the parts of `<name>.data` (see `Stored`; none from lists of 0.4.0).
    #[serde(default)]
    pub sums: Sums,
    /// This list was written anew (with only what the text used) in place of that one: until session.json names this
    /// one instead, one of the two isn't needed (see `finish_rewrites`).
    #[serde(default)]
    pub replaces: Option<String>,
}

impl PiecesHeader {
    /// The head of a list with nothing in it yet.
    fn empty() -> PiecesHeader {
        PiecesHeader { tab: None, len: 0, data_len: 0, files: Vec::new(), sums: Vec::new(), replaces: None }
    }
}

const PIECES_MAGIC: &[u8] = b"SLATE-PIECES 1\n";

/// `<name>.pieces`: a line naming the format, the header as one line of JSON, the pieces (count, then source,
/// start and length of each, little-endian), and a hash of all that (a damaged file is never taken for a list).
fn encode_pieces(h: &PiecesHeader, pieces: &[(u32, u64, u64)]) -> Option<Vec<u8>> {
    let mut out = PIECES_MAGIC.to_vec();
    out.extend(serde_json::to_vec(h).ok()?);
    out.push(b'\n');
    out.extend((pieces.len() as u64).to_le_bytes());
    for &(s, start, len) in pieces {
        out.extend(s.to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(len.to_le_bytes());
    }
    let sum = stable_hash(&out);
    out.extend(sum.to_le_bytes());
    Some(out)
}

/// A document's pieces: (source, start, length) each, in order.
type Pieces = Vec<(u32, u64, u64)>;

fn decode_pieces(b: &[u8]) -> Option<(PiecesHeader, Pieces)> {
    let (body, sum) = b.split_at(b.len().checked_sub(8)?);
    if stable_hash(body) != u64::from_le_bytes(sum.try_into().ok()?) {
        return None;
    }
    let rest = body.strip_prefix(PIECES_MAGIC)?;
    let end = memchr::memchr(b'\n', rest)?;
    let header: PiecesHeader = serde_json::from_slice(&rest[..end]).ok()?;
    let r = &rest[end + 1..];
    let n = u64::from_le_bytes(r.get(..8)?.try_into().ok()?) as usize;
    let r = &r[8..];
    if r.len() != n.checked_mul(20)? {
        return None;
    }
    let pieces = r
        .as_chunks::<20>()
        .0
        .iter()
        .map(|c| {
            let s = u32::from_le_bytes(c[..4].try_into().unwrap());
            let start = u64::from_le_bytes(c[4..12].try_into().unwrap());
            let len = u64::from_le_bytes(c[12..20].try_into().unwrap());
            (s, start, len)
        })
        .collect();
    Some((header, pieces))
}

/// Where parts of sources are in `<name>.data`: per source, (from, to, where in it).
type Places = HashMap<*const Source, Vec<(u64, u64, u64)>>;

/// Works out a big document's write (on the UI thread) from a snapshot: what to add to `<name>.data` (see `Keep`;
/// each only once) and its new list of pieces. None if it can't be kept (`big_keepable`). When most of
/// `<name>.data` isn't used any more, it's written anew under a new name with only what the text uses.
fn plan_big(b: &BigBackup, doc: &mut Document, tab: u64, st: &SessionTab) -> Option<BigWrite> {
    let snap = doc.snapshot();
    let mut kinds: HashMap<*const Source, Keep> = HashMap::new();
    for (_, p) in snap.pieces() {
        let src = &snap.sources()[p.src as usize];
        kinds.entry(Arc::as_ptr(src)).or_insert_with(|| keep_of(src));
    }
    let kind = |src: &Arc<Source>| &kinds[&Arc::as_ptr(src)];
    let (mut used, mut copied) = (0u64, 0u64);
    for (_, p) in snap.pieces() {
        match kind(&snap.sources()[p.src as usize]) {
            Keep::File(_) => {}
            Keep::Whole => used += p.len,
            Keep::Parts => {
                used += p.len;
                copied += p.len;
            }
        }
    }
    if copied > COPY_LIMIT {
        return None;
    }
    let anew = b.listed && b.stored.len > used + used.max(SLACK);
    let (name, data_from) = if anew { (BigBackup::new().name, 0) } else { (b.name.clone(), b.stored.len) };
    // Where what `<name>.data` will hold is: per source, (from, to, where in it).
    let mut at_of: Places = HashMap::new();
    if !anew {
        for (w, from, to, at) in &b.stored.parts {
            if w.strong_count() > 0 {
                at_of.entry(w.as_ptr()).or_default().push((*from, *to, *at));
            }
        }
    }
    let covered = |at_of: &Places, src: &Arc<Source>, a: u64, e: u64| {
        at_of.get(&Arc::as_ptr(src)).is_some_and(|v| v.iter().any(|&(f, t, _)| f <= a && e <= t))
    };
    // What isn't there yet: whole sources, or the parts used (written anew: always only those), per source.
    type Used = (Arc<Source>, Vec<(u64, u64)>);
    let mut add: Vec<(Arc<Source>, u64, u64)> = Vec::new();
    let mut ranges: Vec<Used> = Vec::new();
    for (_, p) in snap.pieces() {
        let src = &snap.sources()[p.src as usize];
        let k = kind(src);
        let (a, e) = (p.start, p.start + p.len);
        if matches!(k, Keep::File(_)) || covered(&at_of, src, a, e) {
            continue;
        }
        if matches!(k, Keep::Whole) && !anew {
            if !add.iter().any(|(s, ..)| Arc::ptr_eq(s, src)) {
                add.push((src.clone(), 0, src.len()));
            }
            continue;
        }
        match ranges.iter_mut().find(|(s, _)| Arc::ptr_eq(s, src)) {
            Some((_, v)) => v.push((a, e)),
            None => ranges.push((src.clone(), vec![(a, e)])),
        }
    }
    for (src, mut v) in ranges {
        v.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (a, e) in v {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((a, e)),
            }
        }
        add.extend(merged.into_iter().map(|(a, e)| (src.clone(), a, e)));
    }
    let mut end = data_from;
    for (s, a, e) in &add {
        at_of.entry(Arc::as_ptr(s)).or_default().push((*a, *e, end));
        end += e - a;
    }
    // The list.
    let mut files: Vec<Identity> = Vec::new();
    let mut named: Vec<Weak<Source>> = Vec::new();
    let mut pieces = Vec::with_capacity(snap.pieces().len());
    for (_, p) in snap.pieces() {
        let src = &snap.sources()[p.src as usize];
        match kind(src) {
            Keep::File(id) => {
                let k = match named.iter().position(|w| w.as_ptr() == Arc::as_ptr(src)) {
                    Some(k) => k,
                    None => {
                        files.push(id.clone());
                        named.push(Arc::downgrade(src));
                        files.len() - 1
                    }
                };
                pieces.push((k as u32 + 1, p.start, p.len));
            }
            _ => {
                let v = &at_of[&Arc::as_ptr(src)];
                let &(from, _, at) = v.iter().find(|&&(f, t, _)| f <= p.start && p.start + p.len <= t)?;
                pieces.push((0, at + p.start - from, p.len));
            }
        }
    }
    let replaces = anew.then(|| b.name.clone());
    let header = PiecesHeader {
        tab: Some(st.clone()),
        len: snap.len(),
        data_len: end,
        files,
        replaces: replaces.clone(),
        ..PiecesHeader::empty()
    };
    Some(BigWrite {
        tab,
        name,
        replaces,
        version: doc.version,
        data_from,
        add,
        sums: if anew { Vec::new() } else { b.stored.sums.clone() },
        header,
        pieces,
        named,
    })
}

/// Writes a big document's part: adds to `<name>.data` and flushes it, then puts the new list of pieces in place
/// (flushed first too). Ok: the length of `<name>.data` in use now, and its hashes.
fn write_big(dir: &Path, w: &BigWrite) -> Result<(u64, Sums), BigErr> {
    // A disk without room for it (and a little more) isn't even tried: nothing is created, nor written again and
    // again.
    let need: u64 = w.add.iter().map(|(_, a, e)| e - a).sum();
    if need > 0 && crate::core::io::free_space(dir).is_some_and(|free| free < need + (need / 8).max(1 << 20)) {
        return Err(BigErr::Failed { full: true });
    }
    let path = dir.join(data_file(&w.name));
    if w.replaces.is_none() {
        return append_big(dir, w, &path);
    }
    // Written anew: under a temporary name until the list that names it is there (a crash on the way leaves nothing
    // that looks like text of its own; see `finish_rewrites`).
    let tmp = dir.join(format!("{}.tmp", data_file(&w.name)));
    let r = append_big(dir, w, &tmp).and_then(|done| match fs::rename(&tmp, &path) {
        Ok(()) => Ok(done),
        Err(_) => Err(BigErr::Failed { full: false }),
    });
    if r.is_err() {
        let _ = fs::remove_file(dir.join(pieces_file(&w.name)));
        let _ = fs::remove_file(&tmp);
    }
    r
}

/// `write_big`'s work: adds to `<name>.data` (at `path`), then writes the list.
fn append_big(dir: &Path, w: &BigWrite, path: &Path) -> Result<(u64, Sums), BigErr> {
    let failed = |e: &std::io::Error| BigErr::Failed { full: matches!(e.raw_os_error(), Some(39 | 112)) };
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false) // (only ever added to, below)
        .share_mode(0x7)
        .open(path)
        .map_err(|e| failed(&e))?;
    let have = f.metadata().map_err(|e| failed(&e))?.len();
    if have < w.data_from {
        return Err(BigErr::Lost);
    }
    let mut end = w.data_from;
    let mut sums = w.sums.clone();
    if !w.add.is_empty() || have > end {
        // (what an interrupted write left after the end goes first)
        f.set_len(end).map_err(|e| failed(&e))?;
        f.seek(SeekFrom::Start(end)).map_err(|e| failed(&e))?;
        let mut out = std::io::BufWriter::with_capacity(1 << 20, &f);
        let mut h = StableHasher::default();
        for (src, a, e) in &w.add {
            let errors = src.read_errors();
            let mut err = None;
            src.chunks(*a, *e, &mut |c| {
                h.update(c);
                match out.write_all(c) {
                    Ok(()) => true,
                    Err(x) => {
                        err = Some(x);
                        false
                    }
                }
            });
            if let Some(x) = err {
                return Err(failed(&x));
            }
            if src.read_errors() != errors {
                return Err(BigErr::Failed { full: false });
            }
            end += e - a;
        }
        out.flush().map_err(|e| failed(&e))?;
        drop(out);
        // On the disk before the list that refers to it.
        f.sync_data().map_err(|e| failed(&e))?;
        if end > w.data_from {
            sums.push((w.data_from, end, h.finish()));
        }
    }
    let header = PiecesHeader { data_len: end, sums: sums.clone(), ..w.header.clone() };
    let list = encode_pieces(&header, &w.pieces).ok_or(BigErr::Failed { full: false })?;
    if !write_flushed(&dir.join(pieces_file(&w.name)), &list) {
        return Err(BigErr::Failed { full: false });
    }
    Ok((end, sums))
}

/// Writes `bytes` to `path` through a temp file flushed to disk before it takes the name.
fn write_flushed(path: &Path, bytes: &[u8]) -> bool {
    let tmp = path.with_extension("tmp");
    let r = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r.is_ok()
}

/// A big document's list of pieces, read back.
pub struct BigList {
    pub header: PiecesHeader,
    pieces: Vec<(u32, u64, u64)>,
}

impl BigList {
    /// The bytes putting it back reads (for progress).
    pub fn total(&self) -> u64 {
        self.header.data_len + self.header.files.iter().map(|f| f.len).sum::<u64>()
    }
}

/// Why a list of pieces can't be read.
#[derive(Debug)]
pub enum ListErr {
    /// It's missing or damaged (set aside, never tried again).
    Damaged(String),
    /// Not just now (another program has it open): try again later.
    Busy(String),
}

/// Reads the list of pieces `<name>.pieces` (this Slate owns the files from now on).
pub fn read_big(name: &str) -> Result<BigList, ListErr> {
    read_big_in(&dir(), name)
}

fn read_big_in(dir: &Path, name: &str) -> Result<BigList, ListErr> {
    if name.contains(['/', '\\']) || name.contains("..") {
        return Err(ListErr::Damaged("its name isn't one Slate gives".into()));
    }
    own(&pieces_file(name));
    own(&data_file(name));
    let bytes = match fs::read(dir.join(pieces_file(name))) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ListErr::Damaged("its list of pieces is missing".into()));
        }
        Err(e) => return Err(ListErr::Busy(crate::core::io::friendly_io(&e))),
    };
    let (header, pieces) =
        decode_pieces(&bytes).ok_or_else(|| ListErr::Damaged("its list of pieces is damaged".into()))?;
    Ok(BigList { header, pieces })
}

/// What putting a big document back came to.
pub enum Restored {
    /// The document as it was, what the session holds of it from now on, and its file's canonical path.
    Ready { doc: Document, kept: BigBackup, canon: Option<PathBuf> },
    /// A file it reads from changed or is gone: the text that was added to it, on its own, and why. `there`: the
    /// file is there (changed), to open as it is now.
    Recovered { doc: Document, why: String, there: bool },
    /// A file it reads from doesn't answer (a network share, a drive that isn't there), or can't be read just now:
    /// try again later.
    Unreachable(String),
    /// The session's files of it can't be read back: why.
    Damaged(String),
}

impl Failure for Restored {
    fn failure(msg: &str) -> Self {
        Restored::Damaged(msg.to_string())
    }
}

impl From<ListErr> for Restored {
    fn from(e: ListErr) -> Restored {
        match e {
            ListErr::Damaged(why) => Restored::Damaged(why),
            ListErr::Busy(why) => Restored::Unreachable(why),
        }
    }
}

/// Puts a big document back from its list (`list`, or read now), on another thread: it reads its files again, and
/// indexes them.
pub fn restore_big(name: &str, list: Option<&BigList>, ctx: &Ctx) -> Restored {
    match list {
        Some(l) => restore_big_in(&dir(), name, l, ctx),
        None => match read_big(name) {
            Ok(l) => restore_big_in(&dir(), name, &l, ctx),
            Err(e) => e.into(),
        },
    }
}

fn restore_big_in(dir: &Path, name: &str, list: &BigList, ctx: &Ctx) -> Restored {
    let h = &list.header;
    let data = match read_data(dir, name, list, ctx) {
        Ok(d) => d,
        Err(e) => return e.into(),
    };
    let mut sources = vec![data.clone().unwrap_or_else(|| Arc::new(Source::from_vec(Vec::new())))];
    let mut done = h.data_len;
    let own_path = h.tab.as_ref().and_then(|t| t.path.as_deref());
    for id in &h.files {
        // (a file other than the document's own, named)
        let which = |why: &str| {
            if own_path == Some(id.path.as_path()) { why.to_string() } else { format!("{}: {why}", name_of(&id.path)) }
        };
        let src = match Source::open_file(&id.path) {
            Ok(s) => s,
            Err(e) if crate::core::io::not_there(&id.path, &e) => {
                return recovered(data.as_ref(), list, which("it isn't there any more"), false);
            }
            // (a drive or folder that isn't there may come back: a USB stick, a share)
            Err(e) => return Restored::Unreachable(crate::core::io::friendly_io(&e)),
        };
        if let Err(why) = src.same_as(id) {
            return recovered(data.as_ref(), list, which(&why), true);
        }
        if !src.build_index_at(&ctx.cancel, &ctx.progress, done) {
            return Restored::Unreachable(src.index_error().unwrap_or_else(|| "cancelled".into()));
        }
        done += id.len;
        sources.push(Arc::new(src));
    }
    let files = sources[1..].to_vec();
    let Some(buf) = Buffer::from_pieces(sources, &list.pieces) else {
        return Restored::Damaged("its list of pieces doesn't fit the files".into());
    };
    if buf.len() != h.len {
        return Restored::Damaged("its list of pieces doesn't fit the files".into());
    }
    let doc = Document::from_buffer(buf);
    let kept = BigBackup::restored(name, data.as_ref(), &files, list, &doc);
    let canon = own_path.and_then(|p| fs::canonicalize(p).ok());
    Restored::Ready { doc, kept, canon }
}

/// `<name>.data`'s first `data_len` bytes (as `list` says) as a source, read once for its index and to check them
/// against the list's hashes. None if the list uses none of it.
fn read_data(dir: &Path, name: &str, list: &BigList, ctx: &Ctx) -> Result<Option<Arc<Source>>, ListErr> {
    use std::io::Read;
    let len = list.header.data_len;
    if len == 0 {
        return Ok(None);
    }
    let missing = || ListErr::Damaged("the text that was added to it is missing".into());
    let path = dir.join(data_file(name));
    let mut file = match OpenOptions::new().read(true).share_mode(0x7).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
        Err(e) => return Err(ListErr::Busy(crate::core::io::friendly_io(&e))),
    };
    if file.metadata().map_or(0, |m| m.len()) < len {
        return Err(missing());
    }
    let mut sums: Sums = list.header.sums.iter().copied().filter(|s| s.1 <= len).collect();
    sums.sort_unstable();
    let (mut k, mut h) = (0, StableHasher::default());
    let mut idx = crate::core::source::IndexBuilder::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut pos = 0u64;
    while pos < len {
        if ctx.cancelled() {
            return Err(ListErr::Busy("cancelled".into()));
        }
        let n = ((len - pos) as usize).min(buf.len());
        if let Err(e) = file.read_exact(&mut buf[..n]) {
            return Err(ListErr::Busy(crate::core::io::friendly_io(&e)));
        }
        idx.push(&buf[..n]);
        // (each hash over what one write added)
        let mut at = pos;
        let end = pos + n as u64;
        while at < end && k < sums.len() {
            let (from, to, want) = sums[k];
            if at < from {
                at = from.min(end);
                continue;
            }
            let upto = to.min(end);
            h.update(&buf[(at - pos) as usize..(upto - pos) as usize]);
            at = upto;
            if at == to {
                if h.finish() != want {
                    return Err(ListErr::Damaged("the text that was added to it is damaged".into()));
                }
                h = StableHasher::default();
                k += 1;
            }
        }
        pos = end;
        ctx.set(pos);
    }
    Ok(Some(Arc::new(Source::session_file(file, len, path, idx.finish()))))
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Gives up waiting for a big document's files (the user asked): the text that was added to it, on its own (on
/// another thread: it reads `<name>.data`).
pub fn recover_big(name: &str, list: Option<&BigList>, ctx: &Ctx) -> Restored {
    let read;
    let list = match list {
        Some(l) => l,
        None => match read_big(name) {
            Ok(l) => {
                read = l;
                &read
            }
            Err(e) => return e.into(),
        },
    };
    match read_data(&dir(), name, list, ctx) {
        Ok(data) => recovered(data.as_ref(), list, "its file didn't answer".into(), false),
        Err(e) => e.into(),
    }
}

/// The text added to a big document whose files changed, on its own: each part with where it was in the edited
/// document (the rest of it was the file's, which isn't what it was any more). Read from `<name>.data` where it is
/// (it can be big), with a line before each part.
fn recovered(data: Option<&Arc<Source>>, list: &BigList, why: String, there: bool) -> Restored {
    let eol: &[u8] = match list.header.tab.as_ref().map(|t| t.eol) {
        Some(Eol::Lf) => b"\n",
        _ => b"\r\n",
    };
    // Source 0 is `<name>.data`, 1 the lines put before the parts.
    let mut heads = Vec::new();
    let mut pieces = Vec::new();
    let mut at = 0u64;
    let mut joined = None;
    for &(s, start, len) in &list.pieces {
        if s == 0 && data.is_some() {
            if joined != Some(at) {
                let from = heads.len() as u64;
                if !pieces.is_empty() {
                    heads.extend_from_slice(eol);
                }
                heads.extend_from_slice(format!("--- added at offset {at} ---").as_bytes());
                heads.extend_from_slice(eol);
                pieces.push((1, from, heads.len() as u64 - from));
            }
            pieces.push((0, start, len));
            joined = Some(at + len);
        }
        at += len;
    }
    let empty = || Arc::new(Source::from_vec(Vec::new()));
    let sources = vec![data.cloned().unwrap_or_else(empty), Arc::new(Source::from_vec(heads))];
    let Some(buf) = Buffer::from_pieces(sources, &pieces) else {
        return Restored::Damaged("its list of pieces doesn't fit the files".into());
    };
    let mut doc = Document::from_buffer(buf);
    doc.eol = if eol == b"\n" { Eol::Lf } else { Eol::Crlf };
    Restored::Recovered { doc, why, there }
}

/// Moves a big document's files into `damaged\` (they're neither put back nor deleted, for `DAMAGED_DAYS`).
/// Returns that folder.
pub fn set_aside_big(name: &str) -> PathBuf {
    set_aside(&pieces_file(name));
    set_aside(&data_file(name));
    dir().join("damaged")
}

/// Finishes what a crash cut short of lists written anew (`PiecesHeader::replaces`), before the session is put back:
/// a new list whose text is all there takes the place of the one it replaces (in `tabs`, session.json's, too), else
/// it goes; the other one goes. And a `<name>.data.tmp` that no list names goes (a write anew that didn't get that
/// far). So none of them comes back as a tab of its own, and the newest text is the one put back.
pub fn finish_rewrites(tabs: &mut [SessionTab]) {
    finish_rewrites_in(&dir(), tabs);
}

fn finish_rewrites_in(d: &Path, tabs: &mut [SessionTab]) {
    let names = |suffix: &str| -> Vec<String> {
        let Ok(rd) = fs::read_dir(d) else { return Vec::new() };
        let mut v: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(suffix).map(str::to_string))
            .filter(|n| n.starts_with("tab-"))
            .collect();
        v.sort();
        v
    };
    let forget = |name: &str| {
        for f in [pieces_file(name), data_file(name), format!("{}.tmp", data_file(name))] {
            let _ = fs::remove_file(d.join(f));
        }
    };
    // (again while that changed something: written anew twice)
    let mut changed = true;
    while changed {
        changed = false;
        for x in names(".pieces") {
            let Some((header, pieces)) = fs::read(d.join(pieces_file(&x))).ok().and_then(|b| decode_pieces(&b)) else {
                continue;
            };
            let Some(w) = header.replaces.clone().filter(|w| *w != x && d.join(pieces_file(w)).exists()) else { continue };
            // (its text is written before the list, under a temporary name until after it)
            let (data, tmp) = (d.join(data_file(&x)), d.join(format!("{}.tmp", data_file(&x))));
            if !data.exists() && tmp.exists() {
                let _ = fs::rename(&tmp, &data);
            }
            // The new list wins only when its text is all there and checks out: a drive that doesn't keep the order
            // of writes (a USB stick) can lose some of it in a power cut, and then the old list is the good copy.
            // Nor when the old list was written after it: then it's a rewrite that didn't finish, left behind.
            let modified = |n: &str| fs::metadata(d.join(pieces_file(n))).and_then(|m| m.modified()).ok();
            let stale = matches!((modified(&w), modified(&x)), (Some(a), Some(b)) if a > b);
            let list = BigList { header, pieces };
            let check = Ctx { cancel: Default::default(), progress: Default::default() };
            let new_wins = !stale
                && match read_data(d, &x, &list, &check) {
                    Ok(_) => true,
                    Err(ListErr::Damaged(_)) => false,
                    // (another program has it open: decided next time)
                    Err(ListErr::Busy(_)) => continue,
                };
            let (keep, drop) = if new_wins { (x, w) } else { (w, x) };
            for t in tabs.iter_mut().filter(|t| t.pieces.as_deref() == Some(drop.as_str())) {
                t.pieces = Some(keep.clone());
            }
            if new_wins {
                forget(&drop);
            } else {
                // (kept for a look, never deleted)
                for f in [pieces_file(&drop), data_file(&drop), format!("{}.tmp", data_file(&drop))] {
                    if d.join(&f).exists() {
                        set_aside_in(d, &f);
                    }
                }
            }
            changed |= !d.join(pieces_file(&drop)).exists();
        }
    }
    for x in names(".data.tmp") {
        if !d.join(pieces_file(&x)).exists() {
            let _ = fs::remove_file(d.join(format!("{}.tmp", data_file(&x))));
        }
    }
}

/// Big documents no restored tab refers to (`claimed`: their names): Slate stopped between writing a list of
/// pieces and the session.json that refers to it. Their lists say what the tabs were.
pub fn big_orphans(claimed: &[String]) -> Vec<SessionTab> {
    big_orphans_in(&dir(), claimed)
}

fn big_orphans_in(d: &Path, claimed: &[String]) -> Vec<SessionTab> {
    let Ok(rd) = fs::read_dir(d) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".pieces").map(str::to_string))
        .filter(|n| n.starts_with("tab-") && !claimed.contains(n))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        match read_big_in(d, &name) {
            Ok(list) => {
                let mut st = list.header.tab.unwrap_or_else(SessionTab::untitled);
                st.backup = None;
                st.pieces = Some(name);
                out.push(st);
            }
            // (kept for a look, never deleted)
            Err(ListErr::Damaged(_)) => {
                set_aside_in(d, &pieces_file(&name));
                set_aside_in(d, &data_file(&name));
            }
            // (next time)
            Err(ListErr::Busy(_)) => {}
        }
    }
    out
}

/// `<name>.data` files too big to come back as a copy (`orphans`) that no list refers to: Slate stopped before it
/// wrote the first one. Each comes back as an untitled big document over all of it (with a list of its own).
pub fn data_orphans() -> Vec<(SessionTab, BigList)> {
    data_orphans_in(&dir())
}

fn data_orphans_in(d: &Path) -> Vec<(SessionTab, BigList)> {
    let Ok(rd) = fs::read_dir(d) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".data").filter(|n| n.starts_with("tab-")) else { continue };
        let len = e.metadata().map_or(0, |m| m.len());
        if len <= BACKUP_LIMIT || d.join(pieces_file(name)).exists() {
            continue;
        }
        own(&file);
        let header = PiecesHeader { len, data_len: len, ..PiecesHeader::empty() };
        let st = SessionTab { pieces: Some(name.to_string()), ..SessionTab::untitled() };
        out.push((st, BigList { header, pieces: vec![(0, 0, len)] }));
    }
    out.sort_by(|a, b| a.0.pieces.cmp(&b.0.pieces));
    out
}

impl SessionTab {
    fn untitled() -> SessionTab {
        SessionTab {
            path: None,
            untitled: 0,
            backup: None,
            pieces: None,
            encoding: Encoding::Utf8,
            bom: false,
            eol: Eol::Crlf,
            lang: Lang::Plain,
            lang_picked: false,
            anchor: 0,
            caret: 0,
            top: 0,
            disk_len: None,
            disk_modified_ms: None,
        }
    }
}

// ---- Tabs that aren't back yet ----

/// A tab from the session that isn't back yet: a big document being put back (it reads its files again), or one
/// whose file doesn't answer (a network share) and is tried again now and then. Until then the session keeps
/// writing `st` for it, as it was read; it stays until the tab is back (also while its file is read, so the tries
/// count on).
pub struct Restoring {
    pub st: SessionTab,
    /// A big document's list of pieces, once read (`is_big` without it: read when it's put back).
    pub big: Option<Arc<BigList>>,
    /// Putting it back, or looking whether its file answers yet.
    pub job: Option<RestoreJob>,
    /// When to look again (it didn't answer), and how many times it didn't.
    pub retry_at: Option<Instant>,
    pub tries: u32,
}

pub enum RestoreJob {
    Big(Job<Restored>),
    /// Some(true): the file is there; Some(false): it isn't any more; None: still no answer.
    Probe(Job<Option<bool>>),
}

impl Restoring {
    pub fn new(st: SessionTab, big: Option<BigList>) -> Restoring {
        Restoring { st, big: big.map(Arc::new), job: None, retry_at: None, tries: 0 }
    }

    /// Didn't answer: when to try again (sooner at first).
    pub fn wait(&mut self) {
        self.tries += 1;
        let secs = [5, 10, 20, 30][(self.tries as usize - 1).min(3)];
        self.retry_at = Some(Instant::now() + Duration::from_secs(secs));
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    /// A big document's unsaved changes (rather than a file to open).
    pub fn is_big(&self) -> bool {
        self.st.pieces.is_some()
    }
}

/// Whether `path` answers: Some(true) it's there, Some(false) it isn't (its folder says so), None: no answer (a
/// network share that's gone, a server that's off, a drive or folder that isn't there).
pub fn probe(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(e) if crate::core::io::not_there(path, &e) => Some(false),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    #[test]
    fn unknown_values_and_bad_tabs_lose_nothing_else() {
        let json = r#"{"tabs": [
            {"path": "C:\\a.txt", "backup": "tab-1.txt", "encoding": "Utf8", "eol": "Lf", "lang": "Zig",
             "anchor": 1, "caret": 2, "top": 0},
            {"path": 5},
            {"path": null, "untitled": 2, "backup": "tab-2.txt", "encoding": "Shift_JIS", "eol": "Cr", "lang": "Json",
             "anchor": 0, "caret": 0, "top": 0, "future_field": [1, 2]}
        ], "active": 1}"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].lang, Lang::Plain);
        assert_eq!(s.tabs[0].eol, Eol::Lf);
        assert_eq!(s.tabs[1].encoding, Encoding::Utf8);
        assert_eq!(s.tabs[1].eol, Eol::Crlf);
        assert_eq!(s.tabs[1].lang, Lang::Json);
        assert_eq!(s.tabs[1].backup.as_deref(), Some("tab-2.txt"));
        // not even a list of tabs: an empty session, not an error
        let s: Session = serde_json::from_str(r#"{"tabs": 7}"#).unwrap();
        assert!(s.tabs.is_empty());
    }

    #[test]
    fn zeroed_backups_are_damaged() {
        assert!(is_damaged(&[0; 100]));
        assert!(!is_damaged(&[]));
        assert!(!is_damaged(b"\0\0a"));
    }

    fn ctx() -> Ctx {
        Ctx { cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)) }
    }

    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("slate-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A document over a file read from disk (as for a big one), indexed.
    fn file_doc(path: &Path, text: &[u8]) -> Document {
        fs::write(path, text).unwrap();
        let src = Arc::new(Source::open_file(path).unwrap());
        assert!(src.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        let mut d = Document::new_pending(src, 0);
        d.path = Some(path.to_path_buf());
        d
    }

    fn edit(d: &mut Document, at: u64, text: &[u8], del: u64) {
        d.begin(crate::core::document::EditKind::Other, Default::default());
        d.delete(at, at + del);
        d.insert(at, text);
        d.end(Default::default());
    }

    /// One write of a big document as the session does it (plan on "the UI thread", write, then what `apply`
    /// does), into `dir`. Ok: `<name>.data`'s length in use after it.
    fn write_once(dir: &Path, big: &mut Option<BigBackup>, d: &mut Document) -> Result<u64, BigErr> {
        let b = big.get_or_insert_with(BigBackup::new);
        let st = SessionTab { path: d.path.clone(), pieces: Some(b.name.clone()), ..SessionTab::untitled() };
        let w = plan_big(b, d, 1, &st).expect("keepable");
        let r = write_big(dir, &w);
        let got = r.clone().map(|(len, sums)| w.stored(len, sums));
        let (name, replaces) = (w.name.clone(), w.replaces.clone());
        apply_big(big, &BigDone { tab: 1, name, replaces, version: w.version, got });
        r.map(|(len, _)| len)
    }

    fn name(big: &Option<BigBackup>) -> String {
        big.as_ref().unwrap().name.clone()
    }

    fn restore_in(dir: &Path, name: &str) -> Restored {
        let list = read_big_in(dir, name).unwrap();
        restore_big_in(dir, name, &list, &ctx())
    }

    fn text_of(r: Restored) -> Vec<u8> {
        match r {
            Restored::Ready { doc, .. } => doc.read(0, doc.len()),
            Restored::Recovered { why, .. } => panic!("recovered: {why}"),
            Restored::Unreachable(why) => panic!("unreachable: {why}"),
            Restored::Damaged(why) => panic!("damaged: {why}"),
        }
    }

    fn lines(n: usize, tag: &str) -> Vec<u8> {
        (0..n).flat_map(|i| format!("{tag} line {i:06}\n").into_bytes()).collect()
    }

    /// What a save of `d` to its file does (written next to it, renamed over it), and what `finish_save` does after:
    /// the text moves onto the new file, and the old one's sources are gone.
    fn save_as_slate_does(d: &mut Document, saved: &Snapshot) {
        let path = d.path.clone().unwrap();
        let tmp = path.with_extension("new");
        let mut bytes = Vec::new();
        saved.chunks(0, saved.len(), &mut |c| {
            bytes.extend_from_slice(c);
            true
        });
        fs::write(&tmp, &bytes).unwrap();
        fs::rename(&tmp, &path).unwrap();
        let new = Arc::new(Source::open_file(&path).unwrap());
        assert!(new.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        for s in d.buffer().sources() {
            if s.file_path() == Some(path.as_path()) {
                s.mark_gone();
            }
        }
        if d.len() == saved.len() && d.read(0, d.len()) == bytes {
            let nl = new.count_nl(0, new.len());
            d.rebase_on(new, 0, nl);
        } else {
            d.rebase_after_save(saved, new, 0);
        }
    }

    #[test]
    fn big_documents_come_back_from_their_pieces_and_writes_only_add() {
        let dir = test_dir("big-session");
        let original = lines(40_000, "orig"); // 680 KB: big enough for many 64 KiB blocks
        let path = dir.join("big.log");
        let mut d = file_doc(&path, &original);
        let mut big = None;
        edit(&mut d, 0, b"typed at the start\n", 0);
        edit(&mut d, 300_000, b"PASTED", 17);
        assert!(big_keepable(&d));
        let first = write_once(&dir, &mut big, &mut d).unwrap();
        let name = name(&big);
        // the next write adds only what was typed since (nothing is written again)
        let end = d.len();
        edit(&mut d, end, b"and at the end\n", 0);
        let second = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(second - first, b"and at the end\n".len() as u64);
        assert_eq!(fs::metadata(dir.join(data_file(&name))).unwrap().len(), second);
        let want = d.read(0, d.len());
        match restore_in(&dir, &name) {
            Restored::Ready { doc, kept, canon } => {
                assert_eq!(doc.read(0, doc.len()), want);
                assert_eq!(doc.line_count(), Some(bytecount::count(&want, b'\n') as u64 + 1));
                assert_eq!(kept.stored.len, second);
                assert_eq!(canon, fs::canonicalize(&path).ok());
                // a document put back goes on from the same files: nothing added again
                let mut doc = doc;
                let mut again = Some(kept);
                let third = write_once(&dir, &mut again, &mut doc).unwrap();
                assert_eq!(third, second);
            }
            _ => panic!("not put back"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_cut_short_leaves_the_last_list() {
        let dir = test_dir("big-cut");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 10, b"first edit", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let name = name(&big);
        let first = d.read(0, d.len());
        // Slate stopped after adding text but before the new list: the old list still holds, and the next write
        // drops what was left after its end
        let mut f = OpenOptions::new().append(true).open(dir.join(data_file(&name))).unwrap();
        f.write_all(b"half written").unwrap();
        drop(f);
        assert_eq!(text_of(restore_in(&dir, &name)), first);
        edit(&mut d, 0, b"second ", 0);
        let len = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(fs::metadata(dir.join(data_file(&name))).unwrap().len(), len);
        assert_eq!(text_of(restore_in(&dir, &name)), d.read(0, d.len()));
        // a damaged list is never taken for one
        let p = dir.join(pieces_file(&name));
        let mut bytes = fs::read(&p).unwrap();
        let k = bytes.len() / 2;
        bytes[k] ^= 1;
        fs::write(&p, bytes).unwrap();
        assert!(matches!(read_big_in(&dir, &name), Err(ListErr::Damaged(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_fails_changes_nothing_and_the_next_one_catches_up() {
        let dir = test_dir("big-fail");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"one ", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let name = name(&big);
        let kept = d.read(0, d.len());
        // can't write (a file that can't be opened for writing): the list on disk stays as it was, and the next
        // write waits a while (more each time)
        let data = dir.join(data_file(&name));
        let mut p = fs::metadata(&data).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&data, p.clone()).unwrap();
        edit(&mut d, 0, b"two ", 0);
        assert_eq!(write_once(&dir, &mut big, &mut d), Err(BigErr::Failed { full: false }));
        assert!(big.as_ref().unwrap().waiting());
        assert_eq!(text_of(restore_in(&dir, &name)), kept);
        assert_eq!(write_once(&dir, &mut big, &mut d), Err(BigErr::Failed { full: false }));
        assert_eq!(big.as_ref().unwrap().retry.unwrap().1, 2);
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(&data, p).unwrap();
        write_once(&dir, &mut big, &mut d).unwrap();
        assert!(!big.as_ref().unwrap().waiting());
        assert_eq!(text_of(restore_in(&dir, &name)), d.read(0, d.len()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_disk_isnt_written_to() {
        let dir = test_dir("big-full");
        // (more than any disk has: never read)
        let huge = 1u64 << 52;
        let (f, path) = crate::core::source::create_temp_file().unwrap();
        let src = Arc::new(Source::session_file(f, huge, path, crate::core::source::IndexBuilder::new().finish()));
        let header = PiecesHeader { len: huge, data_len: huge, ..PiecesHeader::empty() };
        let w = BigWrite {
            tab: 1,
            name: "tab-full".into(),
            replaces: None,
            version: 1,
            data_from: 0,
            add: vec![(src, 0, huge)],
            sums: Vec::new(),
            header,
            pieces: vec![(0, 0, huge)],
            named: Vec::new(),
        };
        assert_eq!(write_big(&dir, &w), Err(BigErr::Failed { full: true }));
        // (not even created)
        assert!(!dir.join("tab-full.data").exists() && !dir.join("tab-full.pieces").exists());
        let mut b = BigBackup::new();
        b.failed(true);
        let first = b.retry.unwrap().0;
        b.failed(true);
        assert!(b.waiting() && b.retry.unwrap().0 > first);
        // nor for a copy (as if it were a small one): nothing created, and the session isn't all written; a copy
        // that fits is written all the same
        let (f, path) = crate::core::source::create_temp_file().unwrap();
        let src = Arc::new(Source::session_file(f, huge, path, crate::core::source::IndexBuilder::new().finish()));
        let mut doc = Document::from_buffer(Buffer::from_source(src, 0));
        let mut small = Document::from_text(b"a few words");
        let backups = vec![
            ("tab-full.txt".to_string(), doc.snapshot(), 1, doc.version),
            ("tab-small.txt".to_string(), small.snapshot(), 2, small.version),
        ];
        let p = Plan { dir: dir.clone(), session: Session::default(), backups, big: Vec::new(), keep: Vec::new(), pending: false };
        let out = write(p);
        assert!(!out.ok);
        assert_eq!(out.written, [(2, "tab-small.txt".to_string(), small.version)]);
        assert!(!dir.join("tab-full.txt").exists() && !dir.join("tab-full.tmp").exists());
        assert_eq!(fs::read(dir.join("tab-small.txt")).unwrap(), b"a few words");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn edits_never_land_on_a_file_that_changed() {
        let dir = test_dir("big-changed");
        let path = dir.join("big.log");
        let mut d = file_doc(&path, &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"MY NOTE\n", 0);
        edit(&mut d, 100_000, b"ANOTHER", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let name = name(&big);
        drop(d);
        // a log that only grew: the edits come back (on the old part)
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"appended later\n").unwrap();
        drop(f);
        assert!(matches!(restore_in(&dir, &name), Restored::Ready { .. }));
        // rewritten by another program: only the added text comes back, on its own (read where it is)
        fs::write(&path, lines(20_000, "new!")).unwrap();
        match restore_in(&dir, &name) {
            Restored::Recovered { doc, why, there } => {
                let text = String::from_utf8(doc.read(0, doc.len())).unwrap();
                assert!(text.contains("MY NOTE") && text.contains("ANOTHER"), "{text}");
                assert!(text.contains("--- added at offset 0 ---"), "{text}");
                assert!(why.contains("changed") && there, "{why}");
                assert!(doc.buffer().sources_in_use().iter().any(|s| s.kind() == SourceKind::Session));
            }
            _ => panic!("edits laid over a changed file"),
        }
        // gone (from a folder that's there)
        fs::remove_file(&path).unwrap();
        assert!(matches!(restore_in(&dir, &name), Restored::Recovered { there: false, .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_drive_or_folder_that_isnt_there_is_waited_for() {
        let dir = test_dir("big-unplugged");
        let stick = dir.join("stick");
        fs::create_dir_all(&stick).unwrap();
        let path = stick.join("big.log");
        let mut d = file_doc(&path, &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"MINE\n", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        drop(d);
        // (as if the USB stick were pulled out: its folder isn't there)
        fs::remove_dir_all(&stick).unwrap();
        assert!(matches!(restore_in(&dir, &name(&big)), Restored::Unreachable(_)));
        assert_eq!(probe(&path), None);
        assert_eq!(probe(&dir.join("nothing.txt")), Some(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn edits_reading_a_file_slate_replaced_are_kept() {
        let dir = test_dir("big-saved");
        let path = dir.join("big.log");
        let original = lines(20_000, "orig");
        let first_line = original.iter().position(|&b| b == b'\n').unwrap() as u64 + 1;
        // A: deleted the first line, saved, undid that: the line is only in the file before the save
        let mut d = file_doc(&path, &original);
        edit(&mut d, 0, b"", first_line);
        let saved = d.snapshot();
        save_as_slate_does(&mut d, &saved);
        assert!(d.undo().is_some());
        assert_eq!(d.read(0, d.len()), original);
        let mut big = None;
        write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(text_of(restore_in(&dir, &name(&big))), original);
        // B: typed while the save ran: the text moves onto the saved file all the same, what was typed aside
        let mut d = file_doc(&path, &original);
        edit(&mut d, 0, b"X", 0);
        let saved = d.snapshot();
        edit(&mut d, 1, b"Y", 0);
        let old = d.buffer().sources_in_use();
        save_as_slate_does(&mut d, &saved);
        assert!(d.buffer().sources_in_use().iter().all(|s| !old.iter().any(|o| Arc::ptr_eq(o, s) && o.is_file())));
        let want = [b"XY".as_slice(), &original].concat();
        assert_eq!(d.read(0, d.len()), want);
        let mut big = None;
        let len = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(len, 1, "only the Y typed after the save is added");
        assert_eq!(text_of(restore_in(&dir, &name(&big))), want);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_can_only_be_copied_is_kept_up_to_a_limit() {
        // a self-deleting temp file (or a file that isn't at its path any more), used in part: copied
        let dir = test_dir("big-parts");
        let text = lines(1000, "converted");
        let (mut f, path) = crate::core::source::create_temp_file().unwrap();
        f.write_all(&text).unwrap();
        let mut b = crate::core::source::IndexBuilder::new();
        b.push(&text);
        let src = Arc::new(Source::from_file(f, text.len() as u64, path, true, Some(b.finish())));
        let nl = src.count_nl(0, src.len());
        let mut d = Document::from_buffer(Buffer::from_source(src, nl));
        edit(&mut d, 0, b"", 1000);
        assert!(big_keepable(&d));
        let mut big = None;
        let len = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(len, text.len() as u64 - 1000, "only the part used");
        drop(d);
        assert_eq!(text_of(restore_in(&dir, &name(&big))), &text[1000..]);
        // too much of it: closing asks
        for temp in [true, false] {
            let (f, path) = crate::core::source::create_temp_file().unwrap();
            let len = COPY_LIMIT + 1;
            let idx = crate::core::source::IndexBuilder::new().finish();
            let src = Arc::new(Source::from_file(f, len, path, temp, Some(idx)));
            src.mark_gone();
            let d = Document::from_buffer(Buffer::from_source(src, 0));
            assert!(!big_keepable(&d));
        }
        assert!(big_keepable(&Document::from_text(b"memory only")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn added_text_is_written_anew_once_most_of_it_is_unused() {
        let dir = test_dir("big-anew");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = None;
        let paste = vec![b'p'; 9 << 20];
        edit(&mut d, 0, &paste, 0);
        edit(&mut d, 0, b"kept ", 0);
        assert!(write_once(&dir, &mut big, &mut d).unwrap() > 9 << 20);
        let before = name(&big);
        // the paste undone: `<name>.data` is mostly unused now
        edit(&mut d, 5, b"", paste.len() as u64);
        let len = write_once(&dir, &mut big, &mut d).unwrap();
        assert_ne!(name(&big), before);
        assert_eq!(len, 5);
        assert_eq!(text_of(restore_in(&dir, &name(&big))), d.read(0, d.len()));
        // put back from a list like that: written anew soon
        let mut list = read_big_in(&dir, &before).unwrap();
        list.pieces.retain(|p| p.0 != 0);
        list.header.len = list.pieces.iter().map(|p| p.2).sum();
        let doc = Document::new();
        assert_eq!(BigBackup::restored(&before, None, &[], &list, &doc).version, u64::MAX);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn added_text_that_was_damaged_isnt_put_back() {
        let dir = test_dir("big-sums");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"some typed text\n", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let data = dir.join(data_file(&name(&big)));
        let mut bytes = fs::read(&data).unwrap();
        bytes[3] ^= 0x20;
        fs::write(&data, bytes).unwrap();
        assert!(matches!(restore_in(&dir, &name(&big)), Restored::Damaged(w) if w.contains("damaged")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_list_another_program_has_open_is_tried_again() {
        let dir = test_dir("big-busy");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"x", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let held = OpenOptions::new().read(true).share_mode(0).open(dir.join(pieces_file(&name(&big)))).unwrap();
        assert!(matches!(read_big_in(&dir, &name(&big)), Err(ListErr::Busy(_))));
        drop(held);
        assert!(read_big_in(&dir, &name(&big)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn big_added_text_without_a_list_comes_back() {
        let dir = test_dir("big-orphan-data");
        let f = fs::File::create(dir.join("tab-1-2-3.data")).unwrap();
        f.set_len(BACKUP_LIMIT + 4096).unwrap();
        drop(f);
        fs::write(dir.join("tab-4-5-6.data"), b"small: comes back as a copy").unwrap();
        let found = data_orphans_in(&dir);
        assert_eq!(found.len(), 1);
        let (st, list) = &found[0];
        assert_eq!(st.pieces.as_deref(), Some("tab-1-2-3"));
        match restore_big_in(&dir, "tab-1-2-3", list, &ctx()) {
            Restored::Ready { doc, .. } => assert_eq!(doc.len(), BACKUP_LIMIT + 4096),
            _ => panic!("not put back"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_files_go_after_a_while() {
        let dir = test_dir("damaged");
        fs::write(dir.join("old.pieces"), b"old").unwrap();
        fs::write(dir.join("new.pieces"), b"new").unwrap();
        let f = OpenOptions::new().write(true).open(dir.join("old.pieces")).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(40 * 24 * 3600)).unwrap();
        drop(f);
        prune_damaged_in(&dir, Duration::from_secs(DAMAGED_DAYS * 24 * 3600));
        assert!(!dir.join("old.pieces").exists());
        assert!(dir.join("new.pieces").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_of_pieces_read_back_as_written() {
        let st = SessionTab { path: Some(PathBuf::from(r"C:\x\big.log")), ..SessionTab::untitled() };
        let sums = vec![(0, 10, u64::MAX)];
        let replaces = Some("tab-before".to_string());
        let h = PiecesHeader { tab: Some(st), len: 30, data_len: 10, sums, replaces, ..PiecesHeader::empty() };
        let pieces = vec![(0, 0, 10), (1, 5, 20)];
        let bytes = encode_pieces(&h, &pieces).unwrap();
        let (back, p) = decode_pieces(&bytes).unwrap();
        assert_eq!(p, pieces);
        assert_eq!((back.len, back.data_len, back.sums), (30, 10, vec![(0, 10, u64::MAX)]));
        assert_eq!(back.replaces.as_deref(), Some("tab-before"));
        assert_eq!(back.tab.unwrap().path, Some(PathBuf::from(r"C:\x\big.log")));
        assert!(decode_pieces(&bytes[..bytes.len() - 1]).is_none());
    }

    /// A tab with a big (over `BACKUP_LIMIT`) document over `dir/big.log` (zeros after a first line), indexed, with
    /// "typed " added at its start.
    fn big_tab(dir: &Path) -> Tab {
        let path = dir.join("big.log");
        let f = fs::File::create(&path).unwrap();
        f.set_len(BACKUP_LIMIT + 4096).unwrap();
        drop(f);
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        std::os::windows::fs::FileExt::seek_write(&f, b"first line\n", 0).unwrap();
        drop(f);
        let src = Arc::new(Source::open_file(&path).unwrap());
        assert!(src.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        let mut doc = Document::new_pending(src, 0);
        doc.path = Some(path);
        let mut t = Tab::new(1, doc);
        edit(&mut t.doc, 0, b"typed ", 0);
        t
    }

    /// One session write the way the timer does it (`closing`: the way closing does), into `dir`.
    fn session_write(dir: &Path, tabs: &mut [Tab], closing: bool) -> Outcome {
        let out = write(plan_in(dir.to_path_buf(), tabs, 0, closing));
        apply(tabs, &out);
        out
    }

    #[test]
    fn a_list_naming_a_file_that_was_replaced_is_written_again() {
        let dir = test_dir("big-due");
        let mut tabs = vec![big_tab(&dir)];
        let out = session_write(&dir, &mut tabs, false);
        assert!(out.ok && out.big.len() == 1);
        let name = name(&tabs[0].big);
        // saved, with nothing typed since: the same version, but the list names the file before the save
        let saved = tabs[0].doc.snapshot();
        save_as_slate_does(&mut tabs[0].doc, &saved);
        assert!(tabs[0].big.as_ref().unwrap().outdated());
        let out = session_write(&dir, &mut tabs, false);
        assert_eq!(out.big.len(), 1, "written again");
        assert!(!tabs[0].big.as_ref().unwrap().outdated());
        let mut want = b"typed first line\n".to_vec();
        want.resize((BACKUP_LIMIT + 4096 + 6) as usize, 0);
        assert_eq!(text_of(restore_in(&dir, &name)), want);
        // nothing new: not written again
        assert!(session_write(&dir, &mut tabs, false).big.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_found_in_place_of_another_makes_the_list_due() {
        let dir = test_dir("big-due-other");
        let path = dir.join("big.log");
        let mut d = file_doc(&path, &lines(20_000, "orig"));
        let mut big = None;
        edit(&mut d, 0, b"x", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        assert!(!big.as_ref().unwrap().outdated());
        fs::write(dir.join("other.log"), b"another file").unwrap();
        fs::rename(dir.join("other.log"), &path).unwrap();
        d.file_sources().iter().for_each(|s| s.look_at_path());
        assert!(big.as_ref().unwrap().outdated());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_waits_is_tried_again() {
        let dir = test_dir("big-wait");
        let mut tabs = vec![big_tab(&dir)];
        // the first write failed: it waits (nothing written yet), kept with its tries; the session isn't all written
        let mut b = BigBackup::new();
        b.failed(false);
        tabs[0].big = Some(b);
        let out = session_write(&dir, &mut tabs, false);
        assert!(out.big.is_empty() && out.pending && !finish(&mut tabs, out));
        assert_eq!(tabs[0].big.as_ref().map(|b| b.retry.unwrap().1), Some(1));
        // closing: tried once more
        let out = session_write(&dir, &mut tabs, true);
        assert!(out.ok && !out.pending && out.big.len() == 1);
        let name = name(&tabs[0].big);
        // waiting with a list written: that list stays in the session meanwhile
        edit(&mut tabs[0].doc, 0, b"more ", 0);
        tabs[0].big.as_mut().unwrap().failed(true);
        let p = plan_in(dir.clone(), &mut tabs, 0, false);
        assert!(p.pending && p.big.is_empty());
        assert_eq!(p.session.tabs[0].pieces.as_deref(), Some(name.as_str()));
        assert!(p.keep.contains(&pieces_file(&name)));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Writes a list `name` (a `replaces` of another, `data_len` bytes of `<name>.data` that it reads).
    fn put_list(dir: &Path, name: &str, replaces: Option<&str>, data: Option<&[u8]>) {
        let len = data.map_or(4, |d| d.len() as u64);
        let replaces = replaces.map(str::to_string);
        let h = PiecesHeader { len, data_len: len, replaces, ..PiecesHeader::empty() };
        fs::write(dir.join(pieces_file(name)), encode_pieces(&h, &[(0, 0, len)]).unwrap()).unwrap();
        if let Some(d) = data {
            fs::write(dir.join(data_file(name)), d).unwrap();
        }
    }

    #[test]
    fn lists_written_anew_when_slate_stopped_are_sorted_out() {
        let dir = test_dir("rewrites");
        let st = |n: &str| SessionTab { pieces: Some(n.to_string()), ..SessionTab::untitled() };
        // whole: it takes the place of the one session.json names (the newest text), which goes
        put_list(&dir, "tab-1", None, Some(b"old!"));
        put_list(&dir, "tab-2", Some("tab-1"), Some(b"new!"));
        // its text still under the temporary name (stopped before the rename): whole too
        put_list(&dir, "tab-3", None, Some(b"old!"));
        put_list(&dir, "tab-4", Some("tab-3"), None);
        fs::write(dir.join("tab-4.data.tmp"), b"new!").unwrap();
        // no text at all: it goes, the one it would replace stays
        put_list(&dir, "tab-5", None, Some(b"old!"));
        put_list(&dir, "tab-6", Some("tab-5"), None);
        // session.json named the new one already (the old one wasn't deleted yet): the old one goes
        put_list(&dir, "tab-7", None, Some(b"old!"));
        put_list(&dir, "tab-8", Some("tab-7"), Some(b"new!"));
        // written anew twice
        put_list(&dir, "tab-a1", None, Some(b"old!"));
        put_list(&dir, "tab-a2", Some("tab-a1"), Some(b"mid!"));
        put_list(&dir, "tab-a3", Some("tab-a2"), Some(b"new!"));
        // a write anew that didn't get to its list
        fs::write(dir.join("tab-9.data.tmp"), b"partial").unwrap();
        let mut tabs = vec![st("tab-1"), st("tab-3"), st("tab-5"), st("tab-8"), st("tab-a1")];
        finish_rewrites_in(&dir, &mut tabs);
        let named: Vec<&str> = tabs.iter().map(|t| t.pieces.as_deref().unwrap()).collect();
        assert_eq!(named, ["tab-2", "tab-4", "tab-5", "tab-8", "tab-a3"]);
        let left = |n: &str| dir.join(pieces_file(n)).exists() || dir.join(data_file(n)).exists();
        for gone in ["tab-1", "tab-3", "tab-6", "tab-7", "tab-a1", "tab-a2"] {
            assert!(!left(gone), "{gone} is left");
        }
        assert_eq!(fs::read(dir.join("tab-4.data")).unwrap(), b"new!");
        assert!(!dir.join("tab-9.data.tmp").exists());
        assert!(big_orphans_in(&dir, &named.iter().map(|n| n.to_string()).collect::<Vec<_>>()).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rewrite_whose_text_didnt_all_reach_the_disk_loses_to_the_old_list() {
        let dir = test_dir("rewrites-bad");
        let st = |n: &str| SessionTab { pieces: Some(n.to_string()), ..SessionTab::untitled() };
        // the new text cut short (a drive that didn't keep the order of writes, then a power cut)
        put_list(&dir, "tab-1", None, Some(b"old!"));
        put_list(&dir, "tab-2", Some("tab-1"), Some(b"new!"));
        fs::write(dir.join(data_file("tab-2")), b"ne").unwrap();
        // the new text all there, but not what was written
        put_list(&dir, "tab-3", None, Some(b"old!"));
        let mut h = StableHasher::default();
        h.update(b"new!");
        let header = PiecesHeader {
            len: 4,
            data_len: 4,
            sums: vec![(0, 4, h.finish())],
            replaces: Some("tab-3".into()),
            ..PiecesHeader::empty()
        };
        fs::write(dir.join(pieces_file("tab-4")), encode_pieces(&header, &[(0, 0, 4)]).unwrap()).unwrap();
        fs::write(dir.join(data_file("tab-4")), b"neW!").unwrap();
        // a rewrite that didn't finish, with the old list written after it: the old one is the newest text
        put_list(&dir, "tab-6", Some("tab-5"), Some(b"mid!"));
        std::thread::sleep(Duration::from_millis(20));
        put_list(&dir, "tab-5", None, Some(b"new!"));
        let mut tabs = vec![st("tab-1"), st("tab-3"), st("tab-5")];
        finish_rewrites_in(&dir, &mut tabs);
        let named: Vec<&str> = tabs.iter().map(|t| t.pieces.as_deref().unwrap()).collect();
        assert_eq!(named, ["tab-1", "tab-3", "tab-5"]);
        for kept in ["tab-1", "tab-3", "tab-5"] {
            assert_eq!(fs::read(dir.join(data_file(kept))).unwrap().len(), 4, "{kept}'s text");
            assert!(dir.join(pieces_file(kept)).exists(), "{kept}'s list");
        }
        // the losers are set aside, never deleted
        for lost in ["tab-2", "tab-4", "tab-6"] {
            assert!(!dir.join(pieces_file(lost)).exists());
            assert!(dir.join("damaged").join(pieces_file(lost)).exists(), "{lost} set aside");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
