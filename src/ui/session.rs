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
//!   written as it was read (`Restoring`), so nothing of it is lost meanwhile.

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
use crate::core::source::{Identity, Source, SourceKind, stable_hash};
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
}

pub struct Outcome {
    /// Everything was written.
    pub ok: bool,
    /// (tab id, file name, document version) of each backup written.
    written: Vec<(u64, String, u64)>,
    /// Each big document: (tab id, name, document version, what `<name>.data` holds now), or Err(true): its files
    /// have to start over, Err(false): it wasn't written this time.
    big: Vec<(u64, String, u64, Result<Stored, bool>)>,
}

impl Failure for Outcome {
    fn failure(_: &str) -> Self {
        Outcome { ok: false, written: Vec::new(), big: Vec::new() }
    }
}

/// Works out what to write. Backups are rewritten only for tabs that changed since the last write.
fn plan(tabs: &mut [Tab], active: usize) -> Plan {
    let d = dir();
    let mut backups = Vec::new();
    let mut big = Vec::new();
    let mut keep: Vec<String> = Vec::new();
    let mut list = Vec::new();
    let mut active_idx = 0;
    for (i, tab) in tabs.iter_mut().enumerate() {
        if tab.load_job.is_some() && tab.doc.path.is_none() {
            continue;
        }
        // Not back yet: as it was (its files are still needed).
        if let Some(r) = &tab.restore {
            keep.extend(files_of(&r.st));
            if i <= active {
                active_idx = list.len();
            }
            list.push(r.st.clone());
            continue;
        }
        let mut backup = None;
        let mut pieces = None;
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
            } else if big_keepable(&tab.doc) {
                let b = tab.big.get_or_insert_with(BigBackup::new);
                let name = b.name.clone();
                pieces = Some(name);
            } else {
                tab.big = None;
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
            let fresh = b.version != tab.doc.version || !d.join(pieces_file(name)).exists();
            let planned = !fresh || {
                own(&pieces_file(name));
                own(&data_file(name));
                plan_big(&mut tab.big, &mut tab.doc, tab.id, &st).map(|w| big.push(w)).is_some()
            };
            if planned {
                keep.extend(files_of(&st));
            } else {
                // (it reads from something that can't be kept after all: closing asks)
                tab.big = None;
                st.pieces = None;
                if st.path.is_none() {
                    continue;
                }
            }
        }
        if i <= active {
            active_idx = list.len();
        }
        list.push(st);
    }
    Plan { dir: d, session: Session { tabs: list, active: active_idx }, backups, big, keep }
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
        return Outcome { ok: false, written: Vec::new(), big: Vec::new() };
    }
    let mut ok = true;
    let mut written = Vec::new();
    for (name, snap, id, version) in p.backups {
        if write_backup(&snap, &p.dir.join(&name)) {
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
            // A list that was never written (or whose added text is gone) can't be put back: the session says
            // nothing about one for this tab (and the next write tries again).
            if r == Err(true) || !p.dir.join(pieces_file(&w.name)).exists() {
                for t in p.session.tabs.iter_mut().filter(|t| t.pieces.as_deref() == Some(w.name.as_str())) {
                    t.pieces = None;
                }
            }
        }
        big.push((w.tab, w.name.clone(), w.version, r.map(|len| w.stored(len))));
    }
    ok &= serde_json::to_vec_pretty(&p.session).is_ok_and(|json| write_session_file(&p.dir, &json));
    // (Otherwise every old backup stays: the session.json on disk may still need them.)
    if ok {
        prune(&p.dir, &p.keep);
    }
    Outcome { ok, written, big }
}

/// Notes which backups are on disk now.
fn apply(tabs: &mut [Tab], out: &Outcome) {
    for (id, name, version) in &out.written {
        if let Some(t) = tabs.iter_mut().find(|t| t.id == *id && t.backup_name.as_deref() == Some(name.as_str())) {
            t.backup_version = *version;
        }
    }
    for (id, name, version, got) in &out.big {
        let Some(t) = tabs.iter_mut().find(|t| t.id == *id) else { continue };
        if t.big.as_ref().is_none_or(|b| &b.name != name) {
            continue;
        }
        match got {
            Ok(s) => t.big.as_mut().unwrap().took(s, *version),
            // `<name>.data` lost text its list refers to: new files next time.
            Err(true) => t.big = None,
            Err(false) => {}
        }
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
    let out = write(plan(tabs, active));
    apply(tabs, &out);
    out.ok
}

/// Starts writing the session on another thread; `finish` takes the outcome.
pub fn start(tabs: &mut [Tab], active: usize, notify: Notify) -> Option<Job<Outcome>> {
    if !super::settings::persist() {
        return None;
    }
    let p = plan(tabs, active);
    Some(Job::spawn(0, notify, move |_| write(p)))
}

/// Takes the outcome of `start`'s write. Returns whether everything was written.
pub fn finish(tabs: &mut [Tab], out: Outcome) -> bool {
    apply(tabs, &out);
    out.ok
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
    let d = dir();
    let _ = fs::create_dir_all(d.join("damaged"));
    let _ = fs::rename(d.join(name), d.join("damaged").join(name));
    OWNED.lock().unwrap().remove(name);
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
// session keeps is its list of pieces, `<name>.pieces` (rewritten when the text changed; small), and the bytes Slate
// added to it (memory: typed and pasted text) in `<name>.data`, each source once, appended and flushed to disk
// before the list that refers to it replaces the old one: a crash or power cut at any point leaves the last list
// and everything it refers to. The rest of the text is read from the user's files, which the list names with their
// `Identity`; putting the document back (`restore_big`) checks that they still hold the same bytes. If they don't
// (another program changed one, or it's gone), the edits are never laid over something else: the text that was
// added comes back on its own in a new tab, and the session's files are moved into `damaged\`.
//
// A document that reads from a self-deleting temp file (the result of Format or Replace all on a big file, a big
// file converted from UTF-16 or ANSI) isn't kept: closing asks about it, as before.

/// What the session holds of a big document.
pub struct BigBackup {
    /// Its files are `<name>.pieces` and `<name>.data`.
    pub name: String,
    /// What `<name>.data` holds (all of it on the disk).
    stored: Stored,
    /// The document version `<name>.pieces` holds (u64::MAX: none yet).
    version: u64,
}

/// The bytes in `<name>.data`: its length in use, and the sources stored in it, at which offset.
#[derive(Clone, Default)]
struct Stored {
    len: u64,
    sources: Vec<(Weak<Source>, u64)>,
}

impl BigBackup {
    fn new() -> BigBackup {
        let name = new_backup_name().trim_end_matches(".txt").to_string();
        BigBackup { name, stored: Stored::default(), version: u64::MAX }
    }

    /// The backup a document (of `version`) was just put back from (`Restored::Ready`).
    pub fn restored(name: String, data: Option<&Arc<Source>>, data_len: u64, version: u64) -> BigBackup {
        let sources = data.iter().map(|d| (Arc::downgrade(d), 0)).collect();
        BigBackup { name, stored: Stored { len: data_len, sources }, version }
    }

    /// Where each source still alive is in `<name>.data`, by address (a `Weak` keeps it from being reused).
    fn offsets(&self) -> HashMap<*const Source, u64> {
        self.stored.sources.iter().filter(|(w, _)| w.strong_count() > 0).map(|(w, o)| (w.as_ptr(), *o)).collect()
    }

    /// A write finished: what `<name>.data` holds now, for document `version`.
    fn took(&mut self, s: &Stored, version: u64) {
        self.stored.len = s.len;
        self.stored.sources.retain(|(w, _)| w.strong_count() > 0);
        self.stored.sources.extend(s.sources.iter().cloned());
        self.version = version;
    }
}

fn pieces_file(name: &str) -> String {
    format!("{name}.pieces")
}

fn data_file(name: &str) -> String {
    format!("{name}.data")
}

/// Whether a big document can be kept as pieces: all it reads is memory, Slate's session files, or files of the
/// user's it can identify again later (not a self-deleting temp file).
pub fn big_keepable(doc: &Document) -> bool {
    doc.buffer().sources_in_use().iter().all(|s| match s.kind() {
        SourceKind::Memory | SourceKind::Session => true,
        SourceKind::File => s.identity().is_some(),
        SourceKind::Temp => false,
    })
}

/// One big document's part of a write.
struct BigWrite {
    tab: u64,
    name: String,
    version: u64,
    /// `<name>.data`'s length in use before (what's after it is left from an interrupted write).
    data_from: u64,
    /// Sources to add to `<name>.data`, one after the other from `data_from`.
    add: Vec<Arc<Source>>,
    /// The new `<name>.pieces`.
    list: Vec<u8>,
}

impl BigWrite {
    /// What `<name>.data` holds once this was written (up to `len`).
    fn stored(&self, len: u64) -> Stored {
        let mut at = self.data_from;
        let mut sources = Vec::new();
        for s in &self.add {
            sources.push((Arc::downgrade(s), at));
            at += s.len();
        }
        Stored { len, sources }
    }
}

/// The head of `<name>.pieces`.
#[derive(Serialize, Deserialize)]
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

/// Works out a big document's write (on the UI thread): the sources to add to `<name>.data` (memory and session
/// files not in it yet, each once) and its new list of pieces. None if it reads from something that can't be kept.
fn plan_big(big: &mut Option<BigBackup>, doc: &mut Document, tab: u64, st: &SessionTab) -> Option<BigWrite> {
    let snap = doc.snapshot();
    let b = big.as_ref()?;
    let mut add: Vec<Arc<Source>> = Vec::new();
    let mut end = b.stored.len;
    let mut at_of = b.offsets();
    let mut files: Vec<Identity> = Vec::new();
    let mut file_ids: Vec<*const Source> = Vec::new();
    let mut pieces = Vec::with_capacity(snap.pieces().len());
    for (_, p) in snap.pieces() {
        let src = &snap.sources()[p.src as usize];
        match src.kind() {
            SourceKind::Memory | SourceKind::Session => {
                let at = *at_of.entry(Arc::as_ptr(src)).or_insert_with(|| {
                    add.push(src.clone());
                    end += src.len();
                    end - src.len()
                });
                pieces.push((0, at + p.start, p.len));
            }
            SourceKind::File => {
                let k = match file_ids.iter().position(|&f| f == Arc::as_ptr(src)) {
                    Some(k) => k,
                    None => {
                        files.push(src.identity()?);
                        file_ids.push(Arc::as_ptr(src));
                        files.len() - 1
                    }
                };
                pieces.push((k as u32 + 1, p.start, p.len));
            }
            SourceKind::Temp => return None,
        }
    }
    let header = PiecesHeader { tab: Some(st.clone()), len: snap.len(), data_len: end, files };
    let list = encode_pieces(&header, &pieces)?;
    Some(BigWrite { tab, name: b.name.clone(), version: doc.version, data_from: b.stored.len, add, list })
}

/// Writes a big document's part: adds the new sources to `<name>.data` and flushes it, then puts the new list of
/// pieces in place (flushed first too). Ok: the length of `<name>.data` in use now. Err(true): `<name>.data` lost
/// text its list refers to (start over with new files); Err(false): couldn't write, try again later.
fn write_big(dir: &Path, w: &BigWrite) -> Result<u64, bool> {
    let path = dir.join(data_file(&w.name));
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false) // (only ever added to, below)
        .share_mode(0x7)
        .open(&path)
        .map_err(|_| false)?;
    let have = f.metadata().map_err(|_| false)?.len();
    if have < w.data_from {
        return Err(true);
    }
    let mut end = w.data_from;
    if !w.add.is_empty() || have > end {
        // (what an interrupted write left after the end goes first)
        f.set_len(end).map_err(|_| false)?;
        f.seek(SeekFrom::Start(end)).map_err(|_| false)?;
        let mut out = std::io::BufWriter::with_capacity(1 << 20, &f);
        for src in &w.add {
            let errors = src.read_errors();
            let mut failed = false;
            src.chunks(0, src.len(), &mut |c| {
                failed = out.write_all(c).is_err();
                !failed
            });
            if failed || src.read_errors() != errors {
                return Err(false);
            }
            end += src.len();
        }
        out.flush().map_err(|_| false)?;
        drop(out);
        // On the disk before the list that refers to it.
        f.sync_data().map_err(|_| false)?;
    }
    write_flushed(&dir.join(pieces_file(&w.name)), &w.list).then_some(end).ok_or(false)
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

/// Reads the list of pieces `<name>.pieces` (this Slate owns the files from now on). Err says why it can't be.
pub fn read_big(name: &str) -> Result<BigList, String> {
    read_big_in(&dir(), name)
}

fn read_big_in(dir: &Path, name: &str) -> Result<BigList, String> {
    if name.contains(['/', '\\']) || name.contains("..") {
        return Err("its name isn't one Slate gives".into());
    }
    own(&pieces_file(name));
    own(&data_file(name));
    let bytes = fs::read(dir.join(pieces_file(name))).map_err(|_| "its list of pieces is missing".to_string())?;
    let (header, pieces) = decode_pieces(&bytes).ok_or_else(|| "its list of pieces is damaged".to_string())?;
    Ok(BigList { header, pieces })
}

/// What putting a big document back came to.
pub enum Restored {
    /// The document as it was; `data` is its source over `<name>.data` (of `data_len` bytes; None if empty).
    Ready { doc: Document, data: Option<Arc<Source>>, data_len: u64 },
    /// A file it reads from changed or is gone: the text that was added to it, on its own, and why. `there`: the
    /// file is there (changed), to open as it is now.
    Recovered { doc: Document, why: String, there: bool },
    /// A file it reads from doesn't answer (a network share), or can't be read just now: try again later.
    Unreachable(String),
    /// The session's files of it can't be read back: why.
    Damaged(String),
}

impl Failure for Restored {
    fn failure(msg: &str) -> Self {
        Restored::Damaged(msg.to_string())
    }
}

/// Puts a big document back from its list (on another thread: it reads its files again, and indexes them).
pub fn restore_big(name: &str, list: &BigList, ctx: &Ctx) -> Restored {
    restore_big_in(&dir(), name, list, ctx)
}

fn restore_big_in(dir: &Path, name: &str, list: &BigList, ctx: &Ctx) -> Restored {
    let h = &list.header;
    let data = if h.data_len > 0 {
        let src = match Source::open_session_file(&dir.join(data_file(name)), h.data_len) {
            Ok(s) => s,
            Err(_) => return Restored::Damaged("the text that was added to it is missing".into()),
        };
        if !src.build_index_at(&ctx.cancel, &ctx.progress, 0) {
            return Restored::Damaged(src.index_error().unwrap_or_else(|| "cancelled".into()));
        }
        Some(Arc::new(src))
    } else {
        None
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
            Err(e) if matches!(e.raw_os_error(), Some(2 | 3)) => {
                return recovered(data.as_ref(), list, which("it isn't there any more"), false);
            }
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
    let Some(buf) = Buffer::from_pieces(sources, &list.pieces) else {
        return Restored::Damaged("its list of pieces doesn't fit the files".into());
    };
    if buf.len() != h.len {
        return Restored::Damaged("its list of pieces doesn't fit the files".into());
    }
    Restored::Ready { doc: Document::from_buffer(buf), data, data_len: h.data_len }
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Gives up waiting for a big document's files (the user asked): the text that was added to it, on its own.
pub fn recover_big(name: &str, list: &BigList) -> Restored {
    let data = match list.header.data_len {
        0 => None,
        n => match Source::open_session_file(&dir().join(data_file(name)), n) {
            Ok(s) => Some(Arc::new(s)),
            Err(_) => return Restored::Damaged("the text that was added to it is missing".into()),
        },
    };
    recovered(data.as_ref(), list, "its file didn't answer".into(), false)
}

/// The text added to a big document whose files changed, on its own: each part with where it was in the edited
/// document (the rest of it was the file's, which isn't what it was any more).
fn recovered(data: Option<&Arc<Source>>, list: &BigList, why: String, there: bool) -> Restored {
    let eol: &[u8] = match list.header.tab.as_ref().map(|t| t.eol) {
        Some(Eol::Lf) => b"\n",
        _ => b"\r\n",
    };
    let mut text = Vec::new();
    let mut at = 0u64;
    let mut joined = None;
    for &(s, start, len) in &list.pieces {
        if s == 0
            && let Some(d) = data
        {
            if joined != Some(at) {
                if !text.is_empty() {
                    text.extend_from_slice(eol);
                }
                text.extend_from_slice(format!("--- added at offset {at} ---").as_bytes());
                text.extend_from_slice(eol);
            }
            d.read_into(start, start + len, &mut text);
            joined = Some(at + len);
        }
        at += len;
    }
    let mut doc = Document::from_text(&text);
    doc.eol = if eol == b"\n" { Eol::Lf } else { Eol::Crlf };
    Restored::Recovered { doc, why, there }
}

/// Moves a big document's files into `damaged\` (they're neither put back nor deleted). Returns that folder.
pub fn set_aside_big(name: &str) -> PathBuf {
    set_aside(&pieces_file(name));
    set_aside(&data_file(name));
    dir().join("damaged")
}

/// Big documents no restored tab refers to (`claimed`: their names): Slate stopped between writing a list of
/// pieces and the session.json that refers to it. Their lists say what the tabs were.
pub fn big_orphans(claimed: &[String]) -> Vec<SessionTab> {
    let d = dir();
    let Ok(rd) = fs::read_dir(&d) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".pieces").map(str::to_string))
        .filter(|n| n.starts_with("tab-") && !claimed.contains(n))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        match read_big_in(&d, &name) {
            Ok(list) => {
                let mut st = list.header.tab.unwrap_or_else(SessionTab::untitled);
                st.backup = None;
                st.pieces = Some(name);
                out.push(st);
            }
            // (kept for a look, never deleted)
            Err(_) => {
                set_aside_big(&name);
            }
        }
    }
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
/// writing `st` for it, as it was read.
pub struct Restoring {
    pub st: SessionTab,
    /// A big document's list of pieces (None: a file to open).
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
}

/// Whether `path` answers: Some(true) it's there, Some(false) it isn't (the folder says so), None: no answer (a
/// network share that's gone, a server that's off).
pub fn probe(path: &Path) -> Option<bool> {
    match fs::metadata(path) {
        Ok(_) => Some(true),
        Err(e) if matches!(e.raw_os_error(), Some(2 | 3)) => Some(false),
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

    /// One write of a big document as the session does it (plan on "the UI thread", then write), into `dir`.
    fn write_once(dir: &Path, big: &mut Option<BigBackup>, d: &mut Document) -> Result<u64, bool> {
        let pieces = big.as_ref().map(|b| b.name.clone());
        let st = SessionTab { path: d.path.clone(), pieces, ..SessionTab::untitled() };
        let w = plan_big(big, d, 1, &st).expect("keepable");
        let r = write_big(dir, &w);
        if let Ok(len) = r {
            big.as_mut().unwrap().took(&w.stored(len), w.version);
        }
        r
    }

    fn restore_in(dir: &Path, name: &str) -> Restored {
        let list = read_big_in(dir, name).unwrap();
        restore_big_in(dir, name, &list, &ctx())
    }

    fn lines(n: usize, tag: &str) -> Vec<u8> {
        (0..n).flat_map(|i| format!("{tag} line {i:06}\n").into_bytes()).collect()
    }

    #[test]
    fn big_documents_come_back_from_their_pieces_and_writes_only_add() {
        let dir = test_dir("big-session");
        let original = lines(40_000, "orig"); // 680 KB: big enough for many 64 KiB blocks
        let mut d = file_doc(&dir.join("big.log"), &original);
        let mut big = Some(BigBackup::new());
        let name = big.as_ref().unwrap().name.clone();
        edit(&mut d, 0, b"typed at the start\n", 0);
        edit(&mut d, 300_000, b"PASTED", 17);
        assert!(big_keepable(&d));
        let first = write_once(&dir, &mut big, &mut d).unwrap();
        // the next write adds only what was typed since (nothing is written again)
        let end = d.len();
        edit(&mut d, end, b"and at the end\n", 0);
        let second = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(second - first, b"and at the end\n".len() as u64);
        assert_eq!(fs::metadata(dir.join(data_file(&name))).unwrap().len(), second);
        let want = d.read(0, d.len());
        match restore_in(&dir, &name) {
            Restored::Ready { doc, data, data_len } => {
                assert_eq!(doc.read(0, doc.len()), want);
                assert_eq!(doc.line_count(), Some(bytecount::count(&want, b'\n') as u64 + 1));
                assert_eq!(data_len, second);
                // a document put back goes on from the same files: nothing added again
                let mut doc = doc;
                let mut again = Some(BigBackup::restored(name.clone(), data.as_ref(), data_len, u64::MAX));
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
        let mut big = Some(BigBackup::new());
        let name = big.as_ref().unwrap().name.clone();
        edit(&mut d, 10, b"first edit", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let first = d.read(0, d.len());
        // Slate stopped after adding text but before the new list: the old list still holds, and the next write
        // drops what was left after its end
        let mut f = OpenOptions::new().append(true).open(dir.join(data_file(&name))).unwrap();
        f.write_all(b"half written").unwrap();
        drop(f);
        match restore_in(&dir, &name) {
            Restored::Ready { doc, .. } => assert_eq!(doc.read(0, doc.len()), first),
            _ => panic!("not put back"),
        }
        edit(&mut d, 0, b"second ", 0);
        let len = write_once(&dir, &mut big, &mut d).unwrap();
        assert_eq!(fs::metadata(dir.join(data_file(&name))).unwrap().len(), len);
        let second = d.read(0, d.len());
        match restore_in(&dir, &name) {
            Restored::Ready { doc, .. } => assert_eq!(doc.read(0, doc.len()), second),
            _ => panic!("not put back"),
        }
        // a damaged list is never taken for one
        let p = dir.join(pieces_file(&name));
        let mut bytes = fs::read(&p).unwrap();
        let k = bytes.len() / 2;
        bytes[k] ^= 1;
        fs::write(&p, bytes).unwrap();
        assert!(read_big_in(&dir, &name).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_fails_changes_nothing_and_the_next_one_catches_up() {
        let dir = test_dir("big-fail");
        let mut d = file_doc(&dir.join("big.log"), &lines(20_000, "orig"));
        let mut big = Some(BigBackup::new());
        let name = big.as_ref().unwrap().name.clone();
        edit(&mut d, 0, b"one ", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        let kept = d.read(0, d.len());
        // can't write (a full disk, a file that can't be opened for writing): the list on disk stays as it was
        let data = dir.join(data_file(&name));
        let mut p = fs::metadata(&data).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&data, p.clone()).unwrap();
        edit(&mut d, 0, b"two ", 0);
        assert_eq!(write_once(&dir, &mut big, &mut d), Err(false));
        match restore_in(&dir, &name) {
            Restored::Ready { doc, .. } => assert_eq!(doc.read(0, doc.len()), kept),
            _ => panic!("not put back"),
        }
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(&data, p).unwrap();
        write_once(&dir, &mut big, &mut d).unwrap();
        match restore_in(&dir, &name) {
            Restored::Ready { doc, .. } => assert_eq!(doc.read(0, doc.len()), d.read(0, d.len())),
            _ => panic!("not put back"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn edits_never_land_on_a_file_that_changed() {
        let dir = test_dir("big-changed");
        let path = dir.join("big.log");
        let mut d = file_doc(&path, &lines(20_000, "orig"));
        let mut big = Some(BigBackup::new());
        let name = big.as_ref().unwrap().name.clone();
        edit(&mut d, 0, b"MY NOTE\n", 0);
        edit(&mut d, 100_000, b"ANOTHER", 0);
        write_once(&dir, &mut big, &mut d).unwrap();
        drop(d);
        // a log that only grew: the edits come back (on the old part)
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"appended later\n").unwrap();
        drop(f);
        assert!(matches!(restore_in(&dir, &name), Restored::Ready { .. }));
        // rewritten by another program: only the added text comes back, on its own
        fs::write(&path, lines(20_000, "new!")).unwrap();
        match restore_in(&dir, &name) {
            Restored::Recovered { doc, why, .. } => {
                let text = String::from_utf8(doc.read(0, doc.len())).unwrap();
                assert!(text.contains("MY NOTE") && text.contains("ANOTHER"), "{text}");
                assert!(text.contains("--- added at offset 0 ---"), "{text}");
                assert!(why.contains("changed"), "{why}");
            }
            _ => panic!("edits laid over a changed file"),
        }
        // gone
        fs::remove_file(&path).unwrap();
        assert!(matches!(restore_in(&dir, &name), Restored::Recovered { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn documents_reading_a_temp_file_are_not_kept_as_pieces() {
        let (mut f, path) = crate::core::source::create_temp_file().unwrap();
        f.write_all(&lines(1000, "converted")).unwrap();
        let len = f.metadata().unwrap().len();
        let mut b = crate::core::source::IndexBuilder::new();
        b.push(&lines(1000, "converted"));
        let src = Arc::new(Source::from_file(f, len, path, true, Some(b.finish())));
        let d = Document::from_buffer(Buffer::from_source(src, 1000));
        assert!(!big_keepable(&d));
        assert!(big_keepable(&Document::from_text(b"memory only")));
    }

    #[test]
    fn lists_of_pieces_read_back_as_written() {
        let st = SessionTab { path: Some(PathBuf::from(r"C:\x\big.log")), ..SessionTab::untitled() };
        let h = PiecesHeader { tab: Some(st), len: 30, data_len: 10, files: Vec::new() };
        let pieces = vec![(0, 0, 10), (1, 5, 20)];
        let bytes = encode_pieces(&h, &pieces).unwrap();
        let (back, p) = decode_pieces(&bytes).unwrap();
        assert_eq!(p, pieces);
        assert_eq!((back.len, back.data_len), (30, 10));
        assert_eq!(back.tab.unwrap().path, Some(PathBuf::from(r"C:\x\big.log")));
        assert!(decode_pieces(&bytes[..bytes.len() - 1]).is_none());
    }
}
