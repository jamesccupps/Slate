//! Remembers open tabs between runs, including unsaved text, like Windows 11 Notepad: closing Slate never asks
//! about unsaved changes for documents up to `BACKUP_LIMIT`; they come back next time. Stored in the data folder
//! (see settings.rs) under `session\` (`session-admin\` for a Slate running as administrator, which runs apart from
//! a normal one): session.json plus one backup file per unsaved tab.
//!
//! This is the user's unsaved work, so:
//! - Files are flushed to disk before they replace the old ones (after a power cut a renamed file can otherwise come
//!   back empty), and the session.json before is kept as session.json.bak.
//! - Reading is lenient: a tab that can't be read, or a value written by a newer Slate, doesn't lose the others.
//! - Slate deletes only backups it wrote or read itself. Backups no tab refers to (session.json couldn't be read,
//!   or Slate stopped between writing a backup and session.json) come back as untitled tabs.
//! - While editing, the session is written on another thread (backups of big documents take a moment); closing and
//!   shutting down write it right away.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::core::buffer::Snapshot;
use crate::core::document::DiskInfo;
use crate::core::job::{Failure, Job, Notify};
use crate::core::text::{Encoding, Eol};

use super::app::Tab;
use super::highlight::Lang;
use super::settings::data_dir;

/// Unsaved documents bigger than this aren't backed up (closing asks instead).
pub const BACKUP_LIMIT: u64 = 64 << 20;

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionTab {
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub untitled: u32,
    #[serde(default)]
    pub backup: Option<String>,
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
    !tab.doc.is_dirty() || (tab.doc.len() <= BACKUP_LIMIT && tab.doc.is_ready())
}

/// One write of the session: worked out on the UI thread, carried out by `write` (there, or on another thread).
pub struct Plan {
    dir: PathBuf,
    session: Session,
    /// Backups to (re)write: file name, the text, and the tab and document version it is.
    backups: Vec<(String, Snapshot, u64, u64)>,
    /// Every backup the session refers to.
    keep: Vec<String>,
}

pub struct Outcome {
    /// Everything was written.
    pub ok: bool,
    /// (tab id, file name, document version) of each backup written.
    written: Vec<(u64, String, u64)>,
}

impl Failure for Outcome {
    fn failure(_: &str) -> Self {
        Outcome { ok: false, written: Vec::new() }
    }
}

/// Works out what to write. Backups are rewritten only for tabs that changed since the last write.
fn plan(tabs: &mut [Tab], active: usize) -> Plan {
    let d = dir();
    let mut backups = Vec::new();
    let mut keep: Vec<String> = Vec::new();
    let mut list = Vec::new();
    let mut active_idx = 0;
    for (i, tab) in tabs.iter_mut().enumerate() {
        if tab.load_job.is_some() && tab.doc.path.is_none() {
            continue;
        }
        let mut backup = None;
        // (Not for a tab the user said "Don't save" to: it comes back as the file on disk, if any.)
        if tab.doc.is_dirty() && !tab.discard && tab.doc.len() <= BACKUP_LIMIT && tab.doc.is_ready() {
            let name = tab.backup_name.get_or_insert_with(new_backup_name).clone();
            if tab.backup_version != tab.doc.version || !d.join(&name).exists() {
                own(&name);
                backups.push((name.clone(), tab.doc.snapshot(), tab.id, tab.doc.version));
            }
            keep.push(name.clone());
            backup = Some(name);
        }
        let doc = &tab.doc;
        if doc.path.is_none() && backup.is_none() {
            // empty untitled tab: nothing to remember
            continue;
        }
        if i <= active {
            active_idx = list.len();
        }
        list.push(SessionTab {
            path: doc.path.clone(),
            untitled: tab.untitled,
            backup,
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
        });
    }
    // Tabs whose files didn't answer this time (a network share): tried again next time.
    list.extend(CARRIED.lock().unwrap().iter().cloned());
    Plan { dir: d, session: Session { tabs: list, active: active_idx }, backups, keep }
}

/// Tabs of the session whose files couldn't be reached this time; kept in the session to be tried again.
static CARRIED: Mutex<Vec<SessionTab>> = Mutex::new(Vec::new());

pub fn carry(t: &SessionTab) {
    CARRIED.lock().unwrap().push(SessionTab { backup: None, ..t.clone() });
}

/// Writes the backups, then the session.json that refers to them, then deletes the backups no longer needed.
fn write(p: Plan) -> Outcome {
    if fs::create_dir_all(&p.dir).is_err() {
        return Outcome { ok: false, written: Vec::new() };
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
    ok &= serde_json::to_vec_pretty(&p.session).is_ok_and(|json| write_session_file(&p.dir, &json));
    // (Otherwise every old backup stays: the session.json on disk may still need them.)
    if ok {
        prune(&p.dir, &p.keep);
    }
    Outcome { ok, written }
}

/// Notes which backups are on disk now.
fn apply(tabs: &mut [Tab], out: &Outcome) {
    for (id, name, version) in &out.written {
        if let Some(t) = tabs.iter_mut().find(|t| t.id == *id && t.backup_name.as_deref() == Some(name.as_str())) {
            t.backup_version = *version;
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
/// now on. Empty ones aren't returned (they're deleted with the next write).
pub fn orphans(claimed: &[String]) -> Vec<(String, Vec<u8>)> {
    let d = dir();
    let Ok(rd) = fs::read_dir(&d) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("tab-") && n.ends_with(".txt") && !claimed.contains(n))
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
    CARRIED.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
