//! The tabs and their unsaved text, kept between runs (like Windows 11 Notepad): `session.json` in the data folder
//! lists the tabs; a tab with unsaved changes (up to `KEEP_MAX`) has its text in a file of its own next to it.
//! Every file is flushed to disk before it replaces the one before, and only files this session wrote are deleted.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::document::Sel;
use crate::core::text::{Encoding, Eol};
use crate::highlight::Lang;
use crate::settings;

use super::app::{App, KEEP_MAX};

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Session {
    tabs: Vec<SessionTab>,
    active: usize,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct SessionTab {
    path: Option<PathBuf>,
    /// The file holding its unsaved text (in the session folder).
    backup: Option<String>,
    caret: u64,
    anchor: u64,
    top: u64,
    lang: Option<Lang>,
    lang_picked: bool,
    encoding: Option<Encoding>,
    bom: bool,
    eol: Option<Eol>,
}

fn dir() -> PathBuf {
    settings::data_dir().join("session")
}

/// Writes `data` to `path` through a temp file that's flushed first, so a crash or power cut leaves the old file
/// or the new one, never half of one.
fn write_safely(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Whether closing can keep every tab's unsaved text (else those tabs are asked about).
pub fn keeps(app: &App, i: usize) -> bool {
    settings::persist() && app.settings.restore_session && app.tabs[i].doc.len() <= KEEP_MAX
}

/// Writes the session now: the tabs, and the text of those with unsaved changes. Returns whether all of it was
/// written.
pub fn save(app: &mut App) -> bool {
    if !settings::persist() {
        return true;
    }
    let dir = dir();
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let mut s = Session { active: app.active, tabs: Vec::new() };
    let mut ok = true;
    let mut written = Vec::new();
    for tab in &mut app.tabs {
        let dirty = tab.doc.is_dirty();
        if !dirty && tab.doc.path.is_none() && tab.doc.is_empty() {
            continue;
        }
        let mut st = SessionTab {
            path: tab.doc.path.clone(),
            caret: tab.view.sel.caret,
            anchor: tab.view.sel.anchor,
            top: tab.view.top,
            lang: Some(tab.lang),
            lang_picked: tab.lang_picked,
            encoding: Some(tab.doc.encoding),
            bom: tab.doc.bom,
            eol: Some(tab.doc.eol),
            backup: None,
        };
        if dirty {
            if !app.settings.restore_session || tab.doc.len() > KEEP_MAX || !tab.doc.is_ready() {
                continue;
            }
            let name = tab.backup.clone().unwrap_or_else(|| format!("tab-{}-{}.txt", std::process::id(), tab.id));
            // (written again only when the text changed since)
            if tab.session_version != tab.doc.version || !dir.join(&name).exists() {
                let text = tab.doc.read(0, tab.doc.len());
                if write_safely(&dir.join(&name), &text).is_err() {
                    ok = false;
                    continue;
                }
                tab.session_version = tab.doc.version;
            }
            tab.backup = Some(name.clone());
            written.push(name.clone());
            st.backup = Some(name);
        } else if tab.doc.path.is_none() {
            continue;
        }
        s.tabs.push(st);
    }
    if !app.settings.restore_session {
        s.tabs.clear();
    }
    let json = serde_json::to_vec_pretty(&s).unwrap_or_default();
    if write_safely(&dir.join("session.json"), &json).is_err() {
        return false;
    }
    // the copies no tab needs any more (only ours: tab-*.txt)
    if ok {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("tab-") && n.ends_with(".txt") && !written.contains(&n) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    app.session_dirty = false;
    ok
}

/// Puts the tabs of the last run back (text tabs at once, files read on other threads).
pub fn restore(app: &mut App) {
    if !app.settings.restore_session {
        return;
    }
    let Ok(bytes) = fs::read(dir().join("session.json")) else { return };
    // (lenient: a damaged file loses the list, never the copies, which stay in the folder)
    let s: Session = serde_json::from_slice(&bytes).unwrap_or_default();
    let mut missing = Vec::new();
    for st in &s.tabs {
        let i = if let Some(name) = &st.backup {
            match fs::read(dir().join(name)) {
                Ok(text) => {
                    let i = app.add_text_tab(&text, st.path.clone(), st.lang);
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
                    if let Some(p) = &st.path {
                        tab.doc.disk = crate::core::io::disk_info(p);
                    }
                    let len = tab.doc.len();
                    tab.view.sel = Sel::new(st.anchor.min(len), st.caret.min(len));
                    tab.view.top = st.top.min(len);
                    i
                }
                Err(_) => {
                    missing.push(st.path.as_ref().map_or("an untitled tab".into(), |p| p.display().to_string()));
                    continue;
                }
            }
        } else if let Some(p) = &st.path {
            if !p.exists() {
                missing.push(p.display().to_string());
                continue;
            }
            app.open_paths(std::slice::from_ref(p));
            let i = app.tabs.len() - 1;
            let tab = &mut app.tabs[i];
            if st.lang_picked {
                if let Some(l) = st.lang {
                    tab.lang = l;
                    tab.lang_picked = true;
                }
            }
            tab.restore_at = Some((st.anchor, st.caret, st.top));
            i
        } else {
            continue;
        };
        let _ = i;
    }
    if !app.tabs.is_empty() {
        app.active = s.active.min(app.tabs.len() - 1);
    }
    if !missing.is_empty() {
        app.flash(format!("Not found any more: {}", missing.join(", ")), true);
    }
}
