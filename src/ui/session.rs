//! Remembers open tabs between runs, including unsaved text, like Windows 11 Notepad: closing Slate never asks
//! about unsaved changes for documents up to `BACKUP_LIMIT`; they come back next time. Stored in
//! %LOCALAPPDATA%\Slate\session\ (session.json plus one backup file per unsaved tab).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::document::{DiskInfo, Document};
use crate::core::text::{Encoding, Eol};

use super::app::Tab;
use super::highlight::Lang;
use super::settings::data_dir;

/// Unsaved documents bigger than this aren't backed up (closing asks instead).
pub const BACKUP_LIMIT: u64 = 64 << 20;

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionTab {
    pub path: Option<PathBuf>,
    pub untitled: u32,
    pub backup: Option<String>,
    pub encoding: Encoding,
    #[serde(default = "yes")]
    pub bom: bool,
    pub eol: Eol,
    pub lang: Lang,
    pub anchor: u64,
    pub caret: u64,
    pub top: u64,
    pub disk_len: Option<u64>,
    pub disk_modified_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: usize,
}

fn yes() -> bool {
    true
}

/// A backup file name no other tab uses, now or in an earlier run.
fn new_backup_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    format!("tab-{:x}-{:x}-{}.txt", t, std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

pub fn dir() -> PathBuf {
    data_dir().join("session")
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

/// Writes the session. Backups are rewritten only for tabs that changed since the last write. Returns false if
/// something couldn't be written.
pub fn save(tabs: &mut [Tab], active: usize) -> bool {
    if !super::settings::persist() {
        return true;
    }
    let d = dir();
    if fs::create_dir_all(&d).is_err() {
        return false;
    }
    let mut ok = true;
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
                if write_backup(&tab.doc, &d.join(&name)) {
                    tab.backup_version = tab.doc.version;
                } else {
                    ok = false;
                }
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
            anchor: tab.view.sel.anchor,
            caret: tab.view.sel.caret,
            top: tab.view.top,
            disk_len: doc.disk.map(|x| x.len),
            disk_modified_ms: doc.disk.map(|x| ms(x.modified)),
        });
    }
    let s = Session { tabs: list, active: active_idx };
    let written = match serde_json::to_vec_pretty(&s) {
        Ok(json) => {
            let tmp = d.join("session.json.tmp");
            fs::write(&tmp, json).is_ok() && fs::rename(&tmp, d.join("session.json")).is_ok()
        }
        Err(_) => false,
    };
    ok &= written;
    if !ok {
        // Keep every old backup: the session file on disk may still need them.
        return false;
    }
    // Remove backups nobody refers to any more.
    if let Ok(rd) = fs::read_dir(&d) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("tab-") && !keep.contains(&name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    ok
}

fn write_backup(doc: &Document, path: &std::path::Path) -> bool {
    let tmp = path.with_extension("tmp");
    let r = (|| -> std::io::Result<()> {
        let f = fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        let mut err = None;
        doc.chunks(0, doc.len(), &mut |c| {
            if let Err(e) = w.write_all(c) {
                err = Some(e);
                return false;
            }
            true
        });
        if let Some(e) = err {
            return Err(e);
        }
        w.flush()?;
        drop(w);
        fs::rename(&tmp, path)
    })();
    r.is_ok()
}

pub fn load() -> Option<Session> {
    let bytes = fs::read(dir().join("session.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn read_backup(name: &str) -> Option<Vec<u8>> {
    if name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    fs::read(dir().join(name)).ok()
}

/// Forgets the session (setting turned off).
pub fn clear() {
    if !super::settings::persist() {
        return;
    }
    let _ = fs::remove_dir_all(dir());
}
