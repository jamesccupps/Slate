//! Input handling, commands, file operations and background jobs.
//!
//! App methods never open modal UI (menus, dialogs) because they run while the App is borrowed; they queue a
//! `Deferred` instead, which `run` performs after the borrow ends.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyMenu, DestroyWindow, GetCaretBlinkTime, GetSystemMetrics, KillTimer, SM_CXDOUBLECLK,
    SPI_GETWHEELSCROLLLINES, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetTimer, SystemParametersInfoW,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, TrackPopupMenuEx,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::core::document::{Document, EditKind, Sel};
use crate::core::io::{self as fileio, Loading, MEM_LIMIT, SaveError};
use crate::core::job::{Ctx as JobCtx, Job};
use crate::core::json::{self, Mode as JsonMode};
use crate::core::lines::{self, CaseOp, LineOp};
use crate::core::xml;
use crate::core::search::{self, Matcher};
use crate::core::source::{IndexBuilder, Source, create_temp_file};
use crate::core::text::{Encoding, Eol};

use super::app::*;
use super::commands::*;
use super::editor::{self, Ctx, DragMode, View};
use super::findbar::{FindBar, Mode as BarMode, Part};
use super::highlight::Lang;
use super::session;
use super::settings::{ThemeMode, data_dir};
use super::win;

pub const WM_APP_JOB: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const TIMER_CARET: usize = 1;
pub const TIMER_JOBS: usize = 2;
pub const TIMER_DISK: usize = 3;
pub const TIMER_SCROLL: usize = 4;
pub const TIMER_SEARCH: usize = 5;
pub const TIMER_SESSION: usize = 6;

/// Documents up to this size are searched on the UI thread (fast enough to feel instant).
const SYNC_SEARCH: u64 = 32 << 20;
const BIG_CLIPBOARD: u64 = 64 << 20;

/// Output for a background transform: memory for small results, a self-deleting temp file for big ones.
enum Sink {
    Mem(Vec<u8>),
    File(BufWriter<File>, PathBuf),
}

impl Sink {
    fn new(len_hint: u64) -> io::Result<Sink> {
        if len_hint <= MEM_LIMIT {
            Ok(Sink::Mem(Vec::with_capacity(len_hint as usize)))
        } else {
            let (f, p) = create_temp_file()?;
            Ok(Sink::File(BufWriter::with_capacity(1 << 20, f), p))
        }
    }

    fn finish(self, idx: IndexBuilder) -> io::Result<(Arc<Source>, u64)> {
        let nl = idx.newlines();
        match self {
            Sink::Mem(v) => Ok((Arc::new(Source::from_vec(v)), nl)),
            Sink::File(w, p) => {
                let f = w.into_inner().map_err(|e| e.into_error())?;
                let len = f.metadata()?.len();
                Ok((Arc::new(Source::from_file(f, len, p, true, Some(idx.finish()))), nl))
            }
        }
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Mem(v) => {
                v.extend_from_slice(buf);
                Ok(buf.len())
            }
            Sink::File(w, _) => w.write(buf),
        }
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Sink::Mem(v) => {
                v.extend_from_slice(buf);
                Ok(())
            }
            Sink::File(w, _) => w.write_all(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Mem(_) => Ok(()),
            Sink::File(w, _) => w.flush(),
        }
    }
}

fn convert_eol(snap: &crate::core::buffer::Snapshot, to_crlf: bool, w: &mut dyn Write, idx: &mut IndexBuilder, ctx: &JobCtx) -> io::Result<u64> {
    let mut out = Vec::with_capacity(1 << 20);
    let mut changed = 0u64;
    let mut prev_cr = false;
    let mut pending_cr = false;
    let mut pos = 0u64;
    let mut err = None;
    while pos < snap.len() {
        if ctx.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let end = (pos + (8 << 20)).min(snap.len());
        snap.chunks(pos, end, &mut |c| {
            for &b in c {
                if to_crlf {
                    if b == b'\n' && !prev_cr {
                        out.push(b'\r');
                        changed += 1;
                    }
                    out.push(b);
                    prev_cr = b == b'\r';
                } else {
                    if pending_cr {
                        pending_cr = false;
                        if b == b'\n' {
                            changed += 1;
                        } else {
                            out.push(b'\r');
                        }
                    }
                    if b == b'\r' {
                        pending_cr = true;
                    } else {
                        out.push(b);
                    }
                }
            }
            if out.len() >= 1 << 20 {
                idx.push(&out);
                if let Err(e) = w.write_all(&out) {
                    err = Some(e);
                    return false;
                }
                out.clear();
            }
            true
        });
        if let Some(e) = err.take() {
            return Err(e);
        }
        pos = end;
        ctx.set(pos);
    }
    if pending_cr {
        out.push(b'\r');
    }
    idx.push(&out);
    w.write_all(&out)?;
    Ok(changed)
}

/// Runs a background transform; if part of the text couldn't be read meanwhile (the file shrank or went away),
/// the result is thrown away instead of producing text with zeros in it.
fn guarded(snap: &crate::core::buffer::Snapshot, f: impl FnOnce() -> TaskResult) -> TaskResult {
    let before = snap.read_errors();
    let r = f();
    if snap.read_errors() != before && !matches!(r, TaskResult::Cancelled) {
        return TaskResult::Failed(
            "Part of the file couldn't be read (was it changed or removed?), so nothing was changed.".into(),
        );
    }
    r
}

fn now_text() -> String {
    unsafe {
        let st = GetLocalTime();
        let mut t = [0u16; 64];
        let mut d = [0u16; 64];
        let nt = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&st), PCWSTR::null(), Some(&mut t));
        let nd = GetDateFormatEx(
            PCWSTR::null(),
            windows::Win32::Globalization::ENUM_DATE_FORMATS_FLAGS(1), // DATE_SHORTDATE
            Some(&st),
            PCWSTR::null(),
            Some(&mut d),
            PCWSTR::null(),
        );
        let t = String::from_utf16_lossy(&t[..(nt.max(1) - 1) as usize]);
        let d = String::from_utf16_lossy(&d[..(nd.max(1) - 1) as usize]);
        format!("{t} {d}")
    }
}

impl App {
    pub fn with_view<R>(&mut self, f: impl FnOnce(&mut View, &Ctx) -> R) -> R {
        let geom = self.editor_geom();
        let tab = &mut self.tabs[self.active];
        let cx = Ctx { doc: &tab.doc, g: &self.g, style: &self.style, theme: &self.theme, lang: tab.lang, geom };
        f(&mut tab.view, &cx)
    }

    pub fn restart_caret(&mut self) {
        self.caret_on = true;
        unsafe {
            SetTimer(self.hwnd, TIMER_CARET, GetCaretBlinkTime().clamp(200, 2000), None);
        }
    }

    pub fn timer(&self, id: usize, ms: u32) {
        unsafe {
            SetTimer(self.hwnd, id, ms, None);
        }
    }

    pub fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(self.hwnd, id);
        }
    }

    fn reveal_caret(&mut self, center: bool) {
        self.with_view(|v, cx| v.reveal(cx, v.sel.caret, center));
    }

    // ---- tabs ----

    pub fn add_tab(&mut self, doc: Document) -> usize {
        let id = self.new_tab_id();
        let mut tab = Tab::new(id, doc);
        let head = tab.doc.read(0, 4096);
        let name = tab.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        tab.lang = Lang::detect(name.as_deref(), &head);
        if tab.doc.path.is_none() {
            let used: Vec<u32> = self.tabs.iter().filter(|t| t.doc.path.is_none()).map(|t| t.untitled).collect();
            tab.untitled = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        }
        self.tabs.push(tab);
        let i = self.tabs.len() - 1;
        self.activate(i);
        self.session_dirty = true;
        i
    }

    pub fn activate(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.active != i {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.view.drag = None;
            }
            // Messages belong to the tab they were about.
            self.flash = None;
        }
        self.active = i;
        self.reveal_active_tab();
        self.update_title();
        if self.find.open {
            self.schedule_count();
        }
        self.restart_caret();
        self.layout();
        self.invalidate();
    }

    pub fn update_title(&self) {
        let t = match self.tabs.get(self.active) {
            Some(tab) => format!("{}{} - Slate", if tab.doc.is_dirty() { "*" } else { "" }, tab.title()),
            None => "Slate".into(),
        };
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(self.hwnd, &HSTRING::from(t));
        }
    }

    pub fn new_untitled(&mut self) {
        let mut doc = Document::new();
        doc.eol = Eol::Crlf;
        self.add_tab(doc);
    }

    /// Closes tab `i` without asking anything.
    pub fn remove_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_untitled();
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        let a = self.active;
        self.activate(a);
        self.session_dirty = true;
    }

    // ---- files ----

    pub fn open_paths(&mut self, paths: &[PathBuf]) {
        for p in paths {
            let p = std::path::absolute(p).unwrap_or_else(|_| p.clone());
            if let Some(i) = self.tabs.iter().position(|t| t.doc.path.as_ref().is_some_and(|q| fileio::same_file(q, &p))) {
                self.activate(i);
                continue;
            }
            // Opening into a single blank tab replaces it, like Notepad.
            let replace_blank = self.tabs.len() == 1 && self.tabs[0].is_blank();
            match fileio::open(&p, self.notify.clone()) {
                Ok(loading) => {
                    let i = match loading {
                        Loading::Ready(doc) => self.add_tab(doc),
                        Loading::Indexing(doc, job) => {
                            let i = self.add_tab(doc);
                            self.tabs[i].index_job = Some(job);
                            i
                        }
                        Loading::Converting(job) => {
                            let mut doc = Document::new();
                            doc.path = Some(p.clone());
                            let i = self.add_tab(doc);
                            self.tabs[i].load_job = Some(job);
                            i
                        }
                    };
                    // Binary files open too (as text), but saving one from here could damage it.
                    if fileio::looks_binary(&p) {
                        self.tabs[i].notice = Some(Notice {
                            kind: NoticeKind::Warn,
                            text: "This looks like a binary file, not text. Saving it from Slate could damage it.".into(),
                            actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                        });
                    }
                    if replace_blank && i == 1 {
                        self.tabs.remove(0);
                        self.active = 0;
                    }
                    self.settings.add_recent(&p);
                    self.timer(TIMER_JOBS, 100);
                }
                Err(e) => {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.flash(format!("Couldn't open {name}: {}", fileio::friendly_io(&e)), true);
                }
            }
        }
        self.settings.save();
        let a = self.active;
        self.activate(a);
    }

    /// Re-reads tab `i` from disk (keeping the view where it was).
    pub fn reload(&mut self, i: usize, force: Option<Encoding>) {
        let Some(path) = self.tabs[i].doc.path.clone() else { return };
        let prev = self.tabs[i].doc.buffer().sources().iter().find(|s| s.file_path() == Some(path.as_path())).cloned();
        let at_end = self.tabs[i].view.sel.caret >= self.tabs[i].doc.len() && self.tabs[i].doc.len() > 0;
        match fileio::open_with(&path, self.notify.clone(), prev, force) {
            Ok(Loading::Ready(doc)) => self.replace_doc(i, doc, None, at_end),
            Ok(Loading::Indexing(doc, job)) => self.replace_doc(i, doc, Some(job), at_end),
            Ok(Loading::Converting(job)) => {
                let tab = &mut self.tabs[i];
                tab.load_job = Some(job);
                tab.reload = true;
                self.timer(TIMER_JOBS, 100);
            }
            Err(e) => {
                self.tabs[i].notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: format!("Couldn't reload the file: {}", fileio::friendly_io(&e)),
                    actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                });
            }
        }
        self.invalidate();
    }

    fn replace_doc(&mut self, i: usize, doc: Document, index_job: Option<Job<bool>>, follow_end: bool) {
        let tab = &mut self.tabs[i];
        let len = doc.len();
        tab.doc = doc;
        tab.index_job = index_job;
        tab.notice = None;
        tab.seen_disk = None;
        tab.search = Search::default();
        tab.structure = Default::default();
        // A different document now: its backup must be written again (and caches rebuilt).
        tab.backup_version = u64::MAX;
        tab.discard = false;
        tab.view.forget_text();
        let v = &mut tab.view;
        v.sel = Sel::new(v.sel.anchor.min(len), v.sel.caret.min(len));
        v.top = v.top.min(len);
        v.upstream = false;
        if follow_end {
            v.sel = Sel::at(len);
        }
        if tab.index_job.is_some() {
            self.timer(TIMER_JOBS, 100);
        }
        if follow_end && i == self.active {
            self.reveal_caret(false);
        }
        self.update_title();
        self.invalidate();
    }

    /// Starts saving tab `i`. Returns false if it can't be saved now.
    pub fn start_save(&mut self, i: usize, path: PathBuf, encoding: Encoding, close_after: bool) -> bool {
        let notify = self.notify.clone();
        let tab = &mut self.tabs[i];
        if let Some(st) = tab.save.as_mut() {
            // Already saving: save the newer text right after (to the same file only).
            if st.path == path {
                st.again |= tab.doc.version != st.version;
                st.close_after |= close_after;
                return true;
            }
            self.flash("Still saving — try again when it's done.", true);
            return false;
        }
        if !tab.doc.is_ready() || tab.load_job.is_some() {
            self.flash("Still opening the file — try saving again in a moment.", true);
            return false;
        }
        let snap = tab.doc.snapshot();
        let state = tab.doc.state_id();
        let version = tab.doc.version;
        let p = path.clone();
        let bom = tab.doc.bom;
        let job = Job::spawn(snap.len(), notify, move |ctx| fileio::save(&snap, &p, encoding, bom, ctx));
        tab.save = Some(SaveTask { job, state, version, path, encoding, close_after, again: false });
        tab.doc.seal();
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
        true
    }

    /// Looks for files changed on disk by other programs.
    pub fn check_disk(&mut self) {
        for i in 0..self.tabs.len() {
            let tab = &self.tabs[i];
            if tab.save.is_some() || tab.load_job.is_some() || tab.task.is_some() {
                continue;
            }
            let (Some(path), Some(old)) = (tab.doc.path.clone(), tab.doc.disk) else { continue };
            let now = fileio::disk_info(&path);
            if now == Some(old) {
                continue;
            }
            if tab.seen_disk == Some(now) {
                continue;
            }
            if now.is_some() && !tab.doc.is_dirty() && tab.index_job.is_none() {
                self.reload(i, None);
                continue;
            }
            let tab = &mut self.tabs[i];
            tab.seen_disk = Some(now);
            tab.notice = Some(if now.is_none() {
                Notice {
                    kind: NoticeKind::Warn,
                    text: "This file was deleted or moved. Save it to keep your copy.".into(),
                    actions: vec![("Save as…".into(), NoticeAction::SaveAs), ("Dismiss".into(), NoticeAction::Dismiss)],
                }
            } else {
                Notice {
                    kind: NoticeKind::Warn,
                    text: "Another program changed this file. Your unsaved changes are still here.".into(),
                    actions: vec![
                        ("Reload (lose my changes)".into(), NoticeAction::Reload),
                        ("Keep mine".into(), NoticeAction::KeepMine),
                    ],
                }
            });
            self.layout();
            self.invalidate();
        }
    }

    // ---- background jobs ----

    pub fn poll_jobs(&mut self) {
        let mut any = false;
        // Finishing a job can close tabs, so go by id.
        let ids: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                any |= self.poll_tab(i);
            }
        }
        if !any {
            self.kill_timer(TIMER_JOBS);
        }
        if self.closing && self.tabs.iter().all(|t| t.save.is_none()) {
            // The saves for closing are done: close (asking about anything still unsaved).
            self.closing = false;
            self.pending.push(Deferred::Cmd(Cmd::Exit));
        }
        self.invalidate();
    }

    /// Closing the window stopped (cancelled, or a save for it failed): earlier "Don't save" answers no longer
    /// count, so the next close asks again.
    pub fn cancel_close(&mut self) {
        self.closing = false;
        for t in &mut self.tabs {
            t.discard = false;
        }
    }

    /// Tabs whose unsaved changes closing now would lose: not kept in the session (too big, or the session
    /// couldn't be written), not being saved, and not let go already.
    pub fn unkept_tabs(&self, session_ok: bool) -> Vec<u64> {
        let keep = self.settings.restore_session && session_ok;
        self.tabs
            .iter()
            .filter(|t| {
                t.doc.is_dirty()
                    && t.save.is_none()
                    && !t.discard
                    && !(keep && session::can_back_up(t))
                    && !(t.doc.is_empty() && t.doc.path.is_none())
            })
            .map(|t| t.id)
            .collect()
    }

    /// Handles finished jobs of tab `i`; returns whether any job is still running.
    fn poll_tab(&mut self, i: usize) -> bool {
        let mut running = false;
        // Loading (conversion of a big UTF-16 / ANSI file).
        if let Some(job) = self.tabs[i].load_job.as_mut() {
            if let Some(r) = job.take() {
                self.tabs[i].load_job = None;
                let reload = std::mem::replace(&mut self.tabs[i].reload, false);
                match r {
                    Ok(doc) => {
                        if reload {
                            self.replace_doc(i, doc, None, false);
                        } else {
                            let tab = &mut self.tabs[i];
                            tab.doc = doc;
                            tab.backup_version = u64::MAX;
                            tab.view.forget_text();
                            let head = tab.doc.read(0, 4096);
                            let name = tab.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                            tab.lang = Lang::detect(name.as_deref(), &head);
                        }
                    }
                    Err(e) => {
                        if reload {
                            self.tabs[i].notice = Some(Notice {
                                kind: NoticeKind::Error,
                                text: format!("Couldn't reload: {}", fileio::friendly_io(&e)),
                                actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                            });
                        } else {
                            let msg = format!("Couldn't open {}: {}", self.tabs[i].title(), fileio::friendly_io(&e));
                            self.remove_tab(i);
                            self.flash(msg, true);
                            return false;
                        }
                    }
                }
            } else {
                running = true;
            }
        }
        // Newline index.
        if let Some(job) = self.tabs[i].index_job.as_mut() {
            if job.take().is_some() {
                self.tabs[i].index_job = None;
                self.tabs[i].doc.poll_index();
                self.tabs[i].view.clear_cache();
            } else {
                running = true;
            }
        }
        // Saving.
        if let Some(st) = self.tabs[i].save.as_mut() {
            if let Some(r) = st.job.take() {
                let st = self.tabs[i].save.take().unwrap();
                if self.finish_save(i, st, r) {
                    return false; // the tab was closed
                }
                running |= self.tabs[i].save.is_some(); // saving again
            } else {
                running = true;
            }
        }
        // Formatting, replacing, converting.
        if let Some(task) = self.tabs[i].task.as_mut() {
            if let Some(r) = task.job.take() {
                let task = self.tabs[i].task.take().unwrap();
                self.finish_task(i, task, r);
            } else {
                running = true;
            }
        }
        // JSON structure scans.
        if self.tabs[i].structure.busy() && !self.tabs[i].structure.poll() {
            running = true;
        }
        // Match counting.
        if let Some(job) = self.tabs[i].search.job.as_mut() {
            if let Some(found) = job.take() {
                self.tabs[i].search.job = None;
                if found.complete {
                    self.tabs[i].search.found = Some(found);
                }
            } else {
                running = true;
            }
        }
        // Find next / previous on a big document.
        if let Some((job, version)) = self.tabs[i].find_job.as_mut() {
            let version = *version;
            if let Some(r) = job.take() {
                self.tabs[i].find_job = None;
                if i == self.active && self.tabs[i].doc.version == version {
                    match r {
                        Some((s, e, wrapped)) => {
                            self.select_match(s, e);
                            if wrapped {
                                self.flash("Search wrapped around", false);
                            }
                        }
                        None => self.flash("No results", true),
                    }
                }
            } else {
                running = true;
            }
        }
        running
    }

    /// Handles a finished save of tab `i`. Returns true if the tab was closed.
    fn finish_save(&mut self, i: usize, st: SaveTask, r: Result<fileio::Saved, SaveError>) -> bool {
        match r {
            Ok(saved) => {
                let tab = &mut self.tabs[i];
                let ext = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
                // A new name, or a new kind of name (notes.txt saved as notes.md): pick the language again.
                let renamed = tab.doc.path.as_deref().map(ext) != Some(ext(&st.path));
                tab.doc.path = Some(st.path.clone());
                tab.doc.encoding = st.encoding;
                tab.doc.disk = saved.disk;
                tab.doc.mark_saved_at(st.state);
                tab.seen_disk = None;
                if tab.doc.version == st.version {
                    if let Some((src, start, nl)) = saved.rebase {
                        tab.doc.rebase_on(src, start, nl);
                    }
                }
                if renamed {
                    let head = tab.doc.read(0, 4096);
                    let name = st.path.file_name().map(|n| n.to_string_lossy().into_owned());
                    tab.lang = Lang::detect(name.as_deref(), &head);
                }
                if matches!(tab.notice.as_ref().map(|n| n.kind), Some(NoticeKind::Error) | Some(NoticeKind::Warn)) {
                    tab.notice = None;
                }
                if saved.lossy {
                    tab.notice = Some(Notice {
                        kind: NoticeKind::Warn,
                        text: "Some characters can't be stored in ANSI and were saved as \"?\".".into(),
                        actions: vec![
                            ("Save as UTF-8".into(), NoticeAction::SaveUtf8),
                            ("Dismiss".into(), NoticeAction::Dismiss),
                        ],
                    });
                }
                self.settings.add_recent(&st.path);
                self.session_dirty = true;
                let dirty = self.tabs[i].doc.is_dirty();
                if st.close_after && (!dirty || self.tabs[i].discard) {
                    self.remove_tab(i);
                    return true;
                }
                if st.again && dirty {
                    let enc = self.tabs[i].doc.encoding;
                    self.start_save(i, st.path.clone(), enc, st.close_after);
                } else if st.close_after {
                    // Typed more while it was saving: closing now would lose that.
                    self.flash("Saved — but you typed more while it was saving, so the tab stayed open.", true);
                } else {
                    self.flash("Saved", false);
                }
                self.update_title();
                false
            }
            Err(e) => {
                self.cancel_close();
                let msg = match &e {
                    SaveError::Cancelled => {
                        self.flash("Saving was cancelled.", false);
                        return false;
                    }
                    SaveError::ReadOnly => "This file is read-only, so it couldn't be saved.".to_string(),
                    SaveError::Io(m) => format!("Couldn't save: {m}"),
                };
                self.tabs[i].notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: msg,
                    actions: vec![("Save as…".into(), NoticeAction::SaveAs), ("Dismiss".into(), NoticeAction::Dismiss)],
                });
                self.layout();
                false
            }
        }
    }

    fn finish_task(&mut self, i: usize, task: Task, r: TaskResult) {
        match r {
            TaskResult::Content { src, nl, count } => {
                let tab = &mut self.tabs[i];
                if tab.doc.version != task.version {
                    self.flash("The text changed while working, so nothing was changed. Try again.", true);
                    return;
                }
                if task.kind == TaskKind::ReplaceAll && count == 0 {
                    self.flash("No matches to replace", true);
                    return;
                }
                if let TaskKind::Lines(op) = task.kind {
                    if count == 0 && !matches!(op, LineOp::SortAsc | LineOp::SortDesc) {
                        self.flash(nothing_to_clean(op), false);
                        return;
                    }
                }
                let sel = tab.view.sel;
                let new_len = src.len();
                tab.doc.begin(EditKind::Other, sel);
                tab.doc.replace_all_with(src, nl);
                let keep_place = matches!(task.kind, TaskKind::ReplaceAll | TaskKind::Lines(_));
                let new_sel = if keep_place { Sel::at(sel.caret.min(new_len)) } else { Sel::at(0) };
                tab.doc.end(new_sel);
                tab.view.sel = new_sel;
                if let TaskKind::Eol(e) = task.kind {
                    tab.doc.eol = e;
                }
                if !keep_place {
                    tab.view.top = 0;
                }
                let msg = match task.kind {
                    TaskKind::Format(_) => "Formatted".to_string(),
                    TaskKind::Minify(_) => "Minified".to_string(),
                    TaskKind::ReplaceAll => format!("Replaced {}", plural(count, "match", "matches")),
                    TaskKind::Eol(e) => format!("Line endings changed to {} ({} lines)", e.short(), group(count)),
                    TaskKind::Lines(op) => lines_done(op, count),
                    TaskKind::Validate(_) => String::new(),
                };
                if i == self.active {
                    self.after_edit();
                }
                self.flash(msg, false);
            }
            TaskResult::Valid(msg) => {
                self.tabs[i].notice = None;
                self.flash(msg, false);
            }
            TaskResult::FormatError { offset, msg } => {
                let what = match task.kind {
                    TaskKind::Format(f) | TaskKind::Minify(f) | TaskKind::Validate(f) => f.name(),
                    _ => "JSON",
                };
                let tab = &mut self.tabs[i];
                let off = offset.min(tab.doc.len());
                let line = tab.doc.line_of(off);
                let col = off - tab.doc.line_start_of(off) + 1;
                let at = match line {
                    Some(l) => format!(" (line {}, column {})", group(l + 1), group(col)),
                    None => String::new(),
                };
                let verb = match task.kind {
                    TaskKind::Validate(_) => format!("Not valid {what}"),
                    _ => format!("Couldn't format: not valid {what}"),
                };
                tab.notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: format!("{verb}: {msg}{at}"),
                    actions: vec![("Show me".into(), NoticeAction::GoTo(off)), ("Dismiss".into(), NoticeAction::Dismiss)],
                });
                if i == self.active {
                    self.go_to(off, true);
                }
                self.layout();
            }
            TaskResult::Failed(m) => {
                self.tabs[i].notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: m,
                    actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                });
                self.layout();
            }
            TaskResult::Cancelled => self.flash("Cancelled", false),
        }
    }

    pub fn start_task(&mut self, kind: TaskKind) {
        let notify = self.notify.clone();
        let changes = !matches!(kind, TaskKind::Validate(_));
        if changes && !self.editable() {
            return;
        }
        let indent = vec![b' '; self.settings.json_indent as usize];
        let replacement = FindBar::text_of(self.find.replace_edit);
        let matcher = self.find.matcher.clone();
        let tab = &mut self.tabs[self.active];
        if tab.task.is_some() {
            self.flash("Still working on the previous task (Esc cancels it).", true);
            return;
        }
        let snap = tab.doc.snapshot();
        let version = tab.doc.version;
        let eol = tab.doc.eol.as_bytes().to_vec();
        let len = snap.len();
        let job = match kind {
            TaskKind::Format(f) | TaskKind::Minify(f) => {
                let pretty = matches!(kind, TaskKind::Format(_));
                let mode = if pretty { JsonMode::Pretty } else { JsonMode::Minify };
                let hint = if pretty { len.saturating_mul(3) } else { len };
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut sink = match Sink::new(hint) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let (w, ix) = (Some(&mut sink as &mut dyn Write), Some(&mut idx));
                    let r = match f {
                        Fmt::Json => json::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg)),
                        Fmt::Xml => xml::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg)),
                    };
                    match r {
                        Ok(()) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Content { src, nl, count: 0 },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                    }
                }))
            }
            TaskKind::Validate(f) => Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                let r = match f {
                    Fmt::Json => json::run(&snap, JsonMode::Validate, b"", b"\n", None, None, ctx)
                        .map(|s| {
                            if s.values > 1 {
                                format!("Valid JSON Lines: {} values", group(s.values))
                            } else {
                                format!("Valid JSON (nesting depth {})", s.max_depth)
                            }
                        })
                        .map_err(|e| (e.offset, e.msg)),
                    Fmt::Xml => xml::run(&snap, JsonMode::Validate, b"", b"\n", None, None, ctx)
                        .map(|s| format!("Valid XML: {} (nesting depth {})", plural(s.elements, "element", "elements"), s.max_depth))
                        .map_err(|e| (e.offset, e.msg)),
                };
                match r {
                    Ok(msg) => TaskResult::Valid(msg),
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                }
            })),
            TaskKind::Lines(op) => {
                if len > LINES_MAX {
                    self.flash("Sorting and cleaning up lines works for files up to 512 MB.", true);
                    return;
                }
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut buf = Vec::new();
                    let text = snap.slice(0, snap.len(), &mut buf);
                    if ctx.cancelled() {
                        return TaskResult::Cancelled;
                    }
                    let (out, count) = lines::apply(op, text);
                    let mut idx = IndexBuilder::new();
                    idx.push(&out);
                    let mut sink = match Sink::new(out.len() as u64) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    if let Err(e) = sink.write_all(&out) {
                        return TaskResult::Failed(format!("Couldn't write the result: {e}"));
                    }
                    match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count },
                        Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    }
                }))
            }
            TaskKind::ReplaceAll => {
                let Some(m) = matcher else {
                    self.flash("Type something to find first", true);
                    return;
                };
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut sink = match Sink::new(len) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    match m.replace_all_to(&snap, replacement.as_bytes(), &mut sink, &mut idx, ctx) {
                        Ok(count) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Content { src, nl, count },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        Err(e) => TaskResult::Failed(format!("Couldn't replace: {e}")),
                    }
                }))
            }
            TaskKind::Eol(e) => Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                let mut sink = match Sink::new(len + len / 16) {
                    Ok(s) => s,
                    Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                };
                let mut idx = IndexBuilder::new();
                match convert_eol(&snap, e == Eol::Crlf, &mut sink, &mut idx, ctx) {
                    Ok(count) => match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count },
                        Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    },
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err(e) => TaskResult::Failed(format!("Couldn't convert: {e}")),
                }
            })),
        };
        tab.task = Some(Task { kind, job, version });
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
    }

    // ---- editing ----

    /// Whether the active document can be edited right now (tells the user why not).
    pub fn editable(&mut self) -> bool {
        let tab = self.tab();
        let why = if tab.load_job.is_some() {
            Some("Still opening the file…")
        } else if !tab.doc.is_ready() {
            Some("Still reading the file's lines — editing works in a moment.")
        } else if tab.task.is_some() {
            Some("Busy with the current task (Esc cancels it).")
        } else {
            None
        };
        if let Some(w) = why {
            self.flash(w, true);
            return false;
        }
        true
    }

    /// After any change to the text.
    pub fn after_edit(&mut self) {
        let tab = &mut self.tabs[self.active];
        tab.discard = false;
        tab.view.sync(&mut tab.doc);
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.search.found = None;
        tab.search.key = None;
        tab.search.job = None;
        self.session_dirty = true;
        self.reveal_caret(false);
        self.restart_caret();
        if self.find.open {
            self.schedule_count();
        }
        self.update_title();
        self.invalidate();
    }

    /// After the caret moved without editing.
    pub fn after_move(&mut self) {
        self.tab_mut().doc.seal();
        self.reveal_caret(false);
        self.restart_caret();
        self.invalidate();
    }

    pub fn type_text(&mut self, s: &str) {
        if s.is_empty() || !self.editable() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let sel = tab.view.sel;
        // Each word is its own undo step.
        if s.starts_with(char::is_whitespace) && sel.is_empty() {
            let before = tab.doc.prev_char(sel.caret);
            let prev = tab.doc.read(before, sel.caret);
            if !prev.is_empty() && !prev.iter().all(|b| b.is_ascii_whitespace()) {
                tab.doc.seal();
            }
        }
        let kind = if sel.is_empty() { EditKind::Typing } else { EditKind::Other };
        tab.view.sel = editor::replace_selection(&mut tab.doc, sel, s.as_bytes(), kind);
        self.after_edit();
    }

    pub fn on_char(&mut self, c: u16) {
        if (0xD800..0xDC00).contains(&c) {
            self.high_surrogate = Some(c);
            return;
        }
        let s = if (0xDC00..0xE000).contains(&c) {
            match self.high_surrogate.take() {
                Some(h) => String::from_utf16_lossy(&[h, c]),
                None => return,
            }
        } else {
            self.high_surrogate = None;
            if c < 0x20 || c == 0x7F {
                return;
            }
            String::from_utf16_lossy(&[c])
        };
        self.type_text(&s);
    }

    /// Key presses in the text area. Returns whether the key was used.
    pub fn on_key(&mut self, vk: u16) -> bool {
        let m = mods();
        if let Some(cmd) = global_key(vk, &m) {
            self.pending.push(Deferred::Cmd(cmd));
            return true;
        }
        let k = VIRTUAL_KEY(vk);
        if m.alt && !m.ctrl && !m.shift {
            if let Some(i) = menu_for_letter(vk) {
                self.pending.push(Deferred::Menu(i));
                return true;
            }
        }
        if k == VK_F10 && !m.ctrl && !m.alt {
            if m.shift {
                self.context_menu_at_caret();
            } else {
                self.pending.push(Deferred::Menu(0));
            }
            return true;
        }
        if k == VK_APPS {
            self.context_menu_at_caret();
            return true;
        }
        if let Some(cmd) = editor_key(vk, &m) {
            self.pending.push(Deferred::Cmd(cmd));
            return true;
        }
        if m.alt {
            return false;
        }
        let ext = m.shift;
        match k {
            VK_LEFT | VK_RIGHT => {
                let right = k == VK_RIGHT;
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                let pos = if !ext && !sel.is_empty() && !m.ctrl {
                    if right { sel.end() } else { sel.start() }
                } else if m.ctrl {
                    if right { tab.doc.word_right(sel.caret) } else { tab.doc.word_left(sel.caret) }
                } else if right {
                    tab.doc.next_char(sel.caret)
                } else {
                    tab.doc.prev_char(sel.caret)
                };
                tab.view.set_caret(pos, ext);
                tab.view.want_x = None;
                self.after_move();
            }
            VK_UP | VK_DOWN => {
                let n = if k == VK_UP { -1 } else { 1 };
                if m.ctrl {
                    self.with_view(|v, cx| v.scroll_rows(cx, n));
                    self.invalidate();
                } else {
                    self.with_view(|v, cx| v.move_rows(cx, n, ext));
                    self.after_move();
                }
            }
            VK_HOME | VK_END => {
                if m.ctrl {
                    let len = self.tab().doc.len();
                    let tab = self.tab_mut();
                    tab.view.set_caret(if k == VK_HOME { 0 } else { len }, ext);
                    tab.view.want_x = None;
                } else if k == VK_HOME {
                    self.with_view(|v, cx| v.home(cx, ext));
                } else {
                    self.with_view(|v, cx| v.end(cx, ext));
                }
                self.after_move();
            }
            VK_PRIOR | VK_NEXT => {
                let down = k == VK_NEXT;
                self.with_view(|v, cx| v.page(cx, down, ext));
                self.after_move();
            }
            VK_BACK => {
                if self.editable() {
                    let tab = self.tab_mut();
                    tab.view.sel = editor::backspace(&mut tab.doc, tab.view.sel, m.ctrl);
                    self.after_edit();
                }
            }
            VK_DELETE => {
                if self.editable() {
                    let tab = self.tab_mut();
                    tab.view.sel = editor::delete_forward(&mut tab.doc, tab.view.sel, m.ctrl);
                    self.after_edit();
                }
            }
            VK_RETURN => {
                if m.ctrl {
                    return false;
                }
                if self.editable() {
                    let unit = self.settings.indent_unit();
                    let tab = self.tab_mut();
                    tab.doc.seal();
                    tab.view.sel = editor::newline(&mut tab.doc, tab.view.sel, &unit);
                    tab.doc.seal();
                    self.after_edit();
                }
            }
            VK_TAB => {
                if m.ctrl {
                    return false;
                }
                self.tab_key(ext);
            }
            VK_ESCAPE => {
                if self.find.open {
                    self.close_find();
                } else if let Some(t) = &self.tab().task {
                    t.job.cancel();
                } else if let Some(s) = &self.tab().save {
                    s.job.cancel();
                } else if self.tab().notice.is_some() {
                    self.tab_mut().notice = None;
                    self.layout();
                    self.invalidate();
                } else {
                    let tab = self.tab_mut();
                    let c = tab.view.sel.caret;
                    tab.view.sel = Sel::at(c);
                    self.invalidate();
                }
            }
            _ => return false,
        }
        true
    }

    fn tab_key(&mut self, shift: bool) {
        if !self.editable() {
            return;
        }
        let unit = self.settings.indent_unit();
        let (tab_size, use_spaces) = (self.settings.tab_size, self.settings.use_spaces);
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        let multi = !sel.is_empty() && tab.doc.line_start_of(sel.start()) != tab.doc.line_start_of(sel.end().saturating_sub(1).max(sel.start()));
        if shift || multi {
            match editor::indent_lines(&mut tab.doc, sel, &unit, tab_size, shift) {
                Some(s) => tab.view.sel = s,
                None => {
                    self.flash("Too many lines selected for that.", true);
                    return;
                }
            }
        } else {
            let text = editor::tab_text(&tab.doc, sel.start(), tab_size, use_spaces);
            tab.doc.seal();
            tab.view.sel = editor::replace_selection(&mut tab.doc, sel, &text, EditKind::Other);
        }
        self.after_edit();
    }

    fn context_menu_at_caret(&mut self) {
        let p = self.with_view(|v, cx| v.caret_point(cx));
        let (x, y) = p.map(|(x, y)| (x, y + self.style.row_h)).unwrap_or((self.r_edit.x + 40.0, self.r_edit.y + 40.0));
        self.pending.push(Deferred::ContextMenu(x, y));
    }

    pub fn select_match(&mut self, s: u64, e: u64) {
        let tab = self.tab_mut();
        tab.view.sel = Sel::new(s, e);
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.doc.seal();
        self.with_view(|v, cx| v.reveal(cx, e, true));
        self.with_view(|v, cx| v.reveal(cx, s, false));
        self.restart_caret();
        self.invalidate();
    }

    /// Moves the caret to `pos` and shows it.
    pub fn go_to(&mut self, pos: u64, center: bool) {
        let tab = self.tab_mut();
        let pos = pos.min(tab.doc.len());
        tab.view.sel = Sel::at(pos);
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.doc.seal();
        self.reveal_caret(center);
        self.restart_caret();
        self.invalidate();
    }

    // ---- clipboard ----

    /// The range Copy/Cut use: the selection, or the whole current line.
    fn copy_range(&self) -> (u64, u64) {
        let tab = self.tab();
        let sel = tab.view.sel;
        if !sel.is_empty() {
            return (sel.start(), sel.end());
        }
        let ls = tab.doc.line_start_of(sel.caret);
        let le = tab.doc.next_newline(sel.caret).map(|p| p + 1).unwrap_or(tab.doc.len());
        (ls, le)
    }

    pub fn copy_size(&self) -> u64 {
        let (a, b) = self.copy_range();
        b - a
    }

    pub fn copy(&mut self) -> bool {
        let (a, b) = self.copy_range();
        if a == b {
            return false;
        }
        let text = self.tab().doc.read(a, b);
        if !win::set_clipboard(self.hwnd, &text) {
            self.flash("Couldn't use the clipboard (another program may be holding it).", true);
            return false;
        }
        true
    }

    pub fn cut(&mut self) {
        if !self.editable() || !self.copy() {
            return;
        }
        let (a, b) = self.copy_range();
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        tab.doc.begin(EditKind::Other, sel);
        tab.doc.delete(a, b);
        tab.doc.end(Sel::at(a));
        tab.view.sel = Sel::at(a);
        self.after_edit();
    }

    pub fn paste(&mut self) {
        if !self.editable() {
            return;
        }
        let Some(text) = win::get_clipboard(self.hwnd) else { return };
        let tab = self.tab_mut();
        let text = editor::normalize_eols(&text, tab.doc.eol.as_bytes());
        // Pasted into an empty new tab: color it like what it looks like (JSON, XML, a script...).
        let guess = tab.doc.is_empty() && tab.doc.path.is_none() && tab.lang == Lang::Plain;
        tab.doc.seal();
        tab.view.sel = editor::replace_selection(&mut tab.doc, tab.view.sel, &text, EditKind::Other);
        tab.doc.seal();
        if guess {
            tab.lang = Lang::detect(None, &text[..text.len().min(4096)]);
        }
        self.after_edit();
    }

    // ---- find bar ----

    pub fn open_find(&mut self, mode: BarMode) {
        self.find.open = true;
        self.find.mode = mode;
        let tab = self.tab();
        let sel = tab.view.sel;
        if mode == BarMode::GoTo {
            self.find.goto_hint = match tab.doc.line_count() {
                Some(n) => format!("1 – {}", group(n)),
                None => "Still reading lines…".into(),
            };
            FindBar::set_text(self.find.goto_edit, "");
            self.layout();
            FindBar::focus(self.find.goto_edit);
        } else {
            if !sel.is_empty() && sel.end() - sel.start() < 300 {
                let b = tab.doc.read(sel.start(), sel.end());
                if !b.contains(&b'\n') {
                    let s = String::from_utf8_lossy(&b).into_owned();
                    FindBar::set_text(self.find.find_edit, &s);
                }
            }
            self.find.origin = Some(sel.start());
            self.layout();
            FindBar::select_all(self.find.find_edit);
            FindBar::focus(self.find.find_edit);
            self.on_find_changed();
            self.schedule_count();
        }
        self.invalidate();
    }

    pub fn close_find(&mut self) {
        self.find.open = false;
        self.layout();
        unsafe {
            let _ = SetFocus(self.hwnd);
        }
        self.invalidate();
    }

    /// The find text or options changed.
    pub fn on_find_changed(&mut self) {
        let text = FindBar::text_of(self.find.find_edit);
        let changed = text != self.find.query.text;
        self.find.query.text = text;
        self.find.compile();
        if changed {
            self.live_search();
        }
        self.schedule_count();
        self.invalidate();
    }

    /// As you type, select the first match at or after where the search started.
    fn live_search(&mut self) {
        let Some(m) = self.find.matcher.clone() else { return };
        let origin = self.find.origin.unwrap_or(self.tab().view.sel.start());
        let len = self.tab().doc.len();
        if len > SYNC_SEARCH {
            self.find_async(m, origin, true);
            return;
        }
        let doc = &self.tab().doc;
        let r = m.find_fwd(doc, origin, len, None).or_else(|| m.find_fwd(doc, 0, origin, None));
        if let Some((s, e)) = r {
            self.select_match(s, e);
        }
    }

    fn find_async(&mut self, m: Arc<Matcher>, from: u64, forward: bool) {
        let notify = self.notify.clone();
        let tab = self.tab_mut();
        let snap = tab.doc.snapshot();
        let version = tab.doc.version;
        let job = Job::spawn(snap.len(), notify, move |ctx| {
            let len = snap.len();
            if forward {
                m.find_fwd(&snap, from, len, Some(ctx)).map(|(s, e)| (s, e, false)).or_else(|| {
                    m.find_fwd(&snap, 0, from, Some(ctx)).map(|(s, e)| (s, e, true))
                })
            } else {
                m.find_back(&snap, 0, from, Some(ctx)).map(|(s, e)| (s, e, false)).or_else(|| {
                    m.find_back(&snap, from, len, Some(ctx)).map(|(s, e)| (s, e, true))
                })
            }
        });
        tab.find_job = Some((job, version));
        self.timer(TIMER_JOBS, 100);
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.find.query.text.is_empty() {
            self.open_find(BarMode::Find);
            return;
        }
        let Some(m) = self.find.matcher.clone() else {
            self.flash(self.find.error.clone().unwrap_or_default(), true);
            return;
        };
        let tab = self.tab();
        let sel = tab.view.sel;
        let len = tab.doc.len();
        let mut from = if forward { sel.end() } else { sel.start() };
        // Don't find the same empty match again.
        if sel.is_empty() && forward && m.is_match_exactly(b"") && from < len {
            from = tab.doc.next_char(from);
        }
        self.find.origin = Some(if forward { sel.end() } else { sel.start() });
        if len > SYNC_SEARCH {
            self.find_async(m, from, forward);
            return;
        }
        let doc = &self.tab().doc;
        let r = if forward {
            m.find_fwd(doc, from, len, None).map(|x| (x, false)).or_else(|| m.find_fwd(doc, 0, from, None).map(|x| (x, true)))
        } else {
            m.find_back(doc, 0, from, None).map(|x| (x, false)).or_else(|| m.find_back(doc, from, len, None).map(|x| (x, true)))
        };
        match r {
            Some(((s, e), wrapped)) => {
                self.select_match(s, e);
                self.find.origin = Some(s);
                if wrapped {
                    self.flash("Search wrapped around", false);
                }
            }
            None => self.flash("No results", true),
        }
    }

    pub fn schedule_count(&mut self) {
        self.timer(TIMER_SEARCH, 180);
    }

    /// Counts all matches in the background (for "3 of 120" and the scrollbar marks).
    pub fn start_count(&mut self) {
        self.kill_timer(TIMER_SEARCH);
        if !self.find.open || self.find.mode == BarMode::GoTo {
            return;
        }
        let Some(m) = self.find.matcher.clone() else {
            let tab = self.tab_mut();
            tab.search = Search::default();
            return;
        };
        let q = &self.find.query;
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let key = (q.text.clone(), q.match_case, q.whole_word, q.regex, tab.doc.version);
        if tab.search.key.as_ref() == Some(&key) {
            return;
        }
        tab.search.key = Some(key);
        tab.search.found = None;
        let snap = tab.doc.snapshot();
        tab.search.job = Some(Job::spawn(snap.len(), notify, move |ctx| search::count_all(&m, &snap, ctx)));
        self.timer(TIMER_JOBS, 100);
    }

    /// "3 of 120" for the find bar.
    pub fn update_find_status(&mut self) {
        let tab = &self.tabs[self.active];
        let sel = tab.view.sel;
        let (msg, bad) = if self.find.query.text.is_empty() {
            (String::new(), false)
        } else if self.find.error.is_some() {
            (String::new(), true)
        } else {
            match &tab.search.found {
                Some(f) if f.count == 0 => ("No results".into(), true),
                Some(f) => match f.positions.binary_search_by_key(&sel.start(), |p| p.0) {
                    Ok(k) if f.positions[k].1 == sel.end() => (format!("{} of {}", group(k as u64 + 1), group(f.count)), false),
                    _ => (format!("{} found", group(f.count)), false),
                },
                None => (if tab.search.job.is_some() { "Searching…".into() } else { String::new() }, false),
            }
        };
        self.find.status = msg;
        self.find.status_bad = bad;
    }

    pub fn replace_one(&mut self) {
        let Some(m) = self.find.matcher.clone() else { return };
        if !self.editable() {
            return;
        }
        let repl = FindBar::text_of(self.find.replace_edit);
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        if !sel.is_empty() && sel.end() - sel.start() < (16 << 20) {
            let text = tab.doc.read(sel.start(), sel.end());
            if m.is_match_exactly(&text) {
                let hs = sel.start().saturating_sub(64);
                let hay = tab.doc.read(hs, (sel.end() + 64).min(tab.doc.len()));
                let mut out = Vec::new();
                m.expand(&hay, (sel.start() - hs) as usize, repl.as_bytes(), &mut out);
                tab.doc.seal();
                tab.view.sel = editor::replace_selection(&mut tab.doc, sel, &out, EditKind::Other);
                tab.doc.seal();
                self.after_edit();
            }
        }
        self.find_next(true);
    }

    pub fn go_to_line_from_bar(&mut self) {
        let text = FindBar::text_of(self.find.goto_edit);
        let t = text.trim();
        let mut parts = t.split([':', ',', ' ']).filter(|s| !s.is_empty());
        let line: Option<u64> = parts.next().and_then(|s| s.replace(['.', '\''], "").parse().ok());
        let col: Option<u64> = parts.next().and_then(|s| s.parse().ok());
        let Some(line) = line else {
            self.flash("Type a line number", true);
            return;
        };
        let tab = self.tab();
        let Some(count) = tab.doc.line_count() else {
            self.flash("Still reading the file's lines — try again in a moment.", true);
            return;
        };
        let line = line.max(1).min(count.max(1));
        let ls = tab.doc.line_start(line - 1).unwrap_or(0);
        let mut pos = ls;
        if let Some(c) = col {
            let le = tab.doc.line_end_of(ls);
            let mut p = ls;
            for _ in 1..c {
                if p >= le {
                    break;
                }
                p = tab.doc.next_char(p);
            }
            pos = p;
        }
        self.close_find();
        self.go_to(pos, true);
    }

    pub fn find_part(&mut self, p: Part) {
        match p {
            Part::Expand => {
                let mode = if self.find.mode == BarMode::Replace { BarMode::Find } else { BarMode::Replace };
                self.find.mode = mode;
                if mode == BarMode::Find && unsafe { GetFocus() } == self.find.replace_edit {
                    // The Replace box is about to be hidden: keep typing in the Find box.
                    FindBar::focus(self.find.find_edit);
                }
                self.layout();
                self.invalidate();
            }
            Part::Case | Part::Word | Part::Regex => {
                let q = &mut self.find.query;
                match p {
                    Part::Case => q.match_case = !q.match_case,
                    Part::Word => q.whole_word = !q.whole_word,
                    _ => q.regex = !q.regex,
                }
                self.find.compile();
                self.tab_mut().search = Search::default();
                self.live_search();
                self.schedule_count();
                self.invalidate();
            }
            Part::Prev => self.find_next(false),
            Part::Next => self.find_next(true),
            Part::Close => self.close_find(),
            Part::ReplaceOne => self.replace_one(),
            Part::ReplaceAll => self.start_task(TaskKind::ReplaceAll),
            Part::Go => self.go_to_line_from_bar(),
        }
    }

    /// Keys typed in the find bar's edit boxes. Returns true if handled (the edit box doesn't see it).
    pub fn bar_key(&mut self, edit: HWND, vk: u16) -> bool {
        let m = mods();
        let k = VIRTUAL_KEY(vk);
        match k {
            VK_RETURN => {
                if edit == self.find.goto_edit {
                    self.go_to_line_from_bar();
                } else if edit == self.find.replace_edit {
                    if m.ctrl && m.alt {
                        self.start_task(TaskKind::ReplaceAll);
                    } else {
                        self.replace_one();
                    }
                } else {
                    self.find_next(!m.shift);
                }
                true
            }
            VK_ESCAPE => {
                self.close_find();
                true
            }
            VK_TAB if !m.ctrl => {
                if self.find.mode == BarMode::Replace {
                    let next = if edit == self.find.find_edit { self.find.replace_edit } else { self.find.find_edit };
                    FindBar::focus(next);
                    FindBar::select_all(next);
                } else {
                    unsafe {
                        let _ = SetFocus(self.hwnd);
                    }
                }
                self.invalidate();
                true
            }
            _ if m.alt && !m.ctrl => {
                let part = match k {
                    VK_C => Some(Part::Case),
                    VK_W => Some(Part::Word),
                    VK_R => Some(Part::Regex),
                    _ => None,
                };
                if let Some(p) = part {
                    self.find_part(p);
                    return true;
                }
                false
            }
            _ => {
                if let Some(cmd) = global_key(vk, &m) {
                    self.pending.push(Deferred::Cmd(cmd));
                    return true;
                }
                false
            }
        }
    }

    // ---- mouse ----

    pub fn on_mouse_down(&mut self, x: f32, y: f32, button: u8) {
        let hit = self.hit(x, y);
        self.down = hit;
        let capture = |h: HWND| unsafe {
            SetCapture(h);
        };
        match (button, hit) {
            (0, Hit::Tab(i)) => {
                self.activate(i);
                self.tab_drag = Some((i, x, false));
                capture(self.hwnd);
            }
            (0, Hit::TabStrip) => {
                if self.click_count(x, y) == 2 {
                    self.pending.push(Deferred::Cmd(Cmd::NewTab));
                }
            }
            (0, Hit::Menu(i)) => self.pending.push(Deferred::Menu(i)),
            (0, Hit::Text) | (0, Hit::Gutter) => {
                unsafe {
                    let _ = SetFocus(self.hwnd);
                }
                let n = self.click_count(x, y);
                self.editor_click(x, y, n, hit == Hit::Gutter);
                capture(self.hwnd);
            }
            (0, Hit::VBar) => {
                self.vbar_down(y);
                capture(self.hwnd);
            }
            (0, Hit::HBar) => {
                self.hbar_down(x);
                capture(self.hwnd);
            }
            (0, Hit::PathSeg(i)) => self.jump_path(i),
            (0, Hit::PathToggle) => self.exec(Cmd::ToggleStructure),
            (0, Hit::StructRow(i, chevron)) => {
                let n = self.click_count(x, y);
                self.struct_click(i, chevron, n == 2);
            }
            (0, Hit::StructClose) => {
                self.settings.structure_panel = false;
                self.settings.save();
                self.layout();
                self.invalidate();
            }
            (0, Hit::StructSplitter) => {
                self.split_drag = Some((x, self.r_struct.w));
                capture(self.hwnd);
            }
            (0, _) => capture(self.hwnd),
            (1, Hit::Text) => {
                // Right-click outside the selection moves the caret there first.
                let (pos, up) = self.with_view(|v, cx| v.pos_at(cx, x, y));
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                if pos < sel.start() || pos > sel.end() || sel.is_empty() {
                    tab.view.set_caret(pos, false);
                    tab.view.upstream = up;
                }
                self.invalidate();
            }
            _ => {}
        }
    }

    fn click_count(&mut self, x: f32, y: f32) -> u32 {
        let (t, lx, ly, n) = self.last_click;
        let ms = unsafe { GetDoubleClickTime() } as u128;
        let slop = self.px_to_dip(unsafe { GetSystemMetrics(SM_CXDOUBLECLK) }).max(4.0);
        let n = if t.elapsed().as_millis() <= ms && (x - lx).abs() <= slop && (y - ly).abs() <= slop { n % 3 + 1 } else { 1 };
        self.last_click = (Instant::now(), x, y, n);
        n
    }

    fn editor_click(&mut self, x: f32, y: f32, clicks: u32, gutter: bool) {
        let ext = mods().shift;
        let (pos, up) = self.with_view(|v, cx| v.pos_at(cx, x, y));
        let tab = self.tab_mut();
        tab.doc.seal();
        tab.view.want_x = None;
        if gutter || clicks == 3 {
            let ls = tab.doc.line_start_of(pos);
            let le = tab.doc.next_newline(pos).map(|p| p + 1).unwrap_or(tab.doc.len());
            tab.view.sel = Sel::new(ls, le);
            tab.view.drag = Some(DragMode::Lines(ls, le));
        } else if clicks == 2 {
            let (a, b) = tab.doc.word_at(pos);
            tab.view.sel = Sel::new(a, b);
            tab.view.drag = Some(DragMode::Words(a, b));
        } else {
            tab.view.set_caret(pos, ext);
            tab.view.upstream = up;
            tab.view.drag = Some(DragMode::Chars);
        }
        self.restart_caret();
        self.invalidate();
    }

    fn vbar_down(&mut self, y: f32) {
        let geom = self.editor_geom();
        let vb = geom.vbar();
        let (frac, shown) = self.with_view(|v, cx| v.scroll_fraction(cx));
        let thumb_h = (vb.h * shown).max(28.0).min(vb.h);
        let ty = vb.y + (vb.h - thumb_h) * frac;
        if y >= ty && y < ty + thumb_h {
            self.tab_mut().view.drag = Some(DragMode::VScroll { grab: (y - ty) as i32 });
        } else {
            let down = y > ty;
            self.with_view(|v, cx| {
                let n = ((cx.geom.rect.h / cx.style.row_h) as i64 - 1).max(1);
                v.scroll_rows(cx, if down { n } else { -n });
            });
            self.invalidate();
        }
    }

    fn hbar_down(&mut self, x: f32) {
        let geom = self.editor_geom();
        let hb = geom.hbar();
        let tab = self.tab();
        let total = tab.view.content_w + 40.0;
        let thumb_w = (hb.w * geom.text_w / total).max(28.0).min(hb.w);
        let max = (total - geom.text_w).max(1.0);
        let tx = hb.x + (hb.w - thumb_w) * (tab.view.scroll_x / max).clamp(0.0, 1.0);
        if x >= tx && x < tx + thumb_w {
            self.tab_mut().view.drag = Some(DragMode::HScroll { grab: (x - tx) as i32 });
        } else {
            let step = geom.text_w * 0.8;
            let v = &mut self.tab_mut().view;
            v.scroll_x = if x > tx { (v.scroll_x + step).min(max) } else { (v.scroll_x - step).max(0.0) };
            self.invalidate();
        }
    }

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        if !self.mouse_tracking {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
            self.mouse_tracking = true;
        }
        if let Some((x0, w0)) = self.split_drag {
            self.settings.structure_width = (w0 - (x - x0)).clamp(200.0, (self.size.0 * 0.6).max(200.0));
            self.layout();
            self.invalidate();
            return;
        }
        if let Some(drag) = self.tab().view.drag {
            self.drag_to(drag, x, y);
            return;
        }
        if let Some((i, x0, moved)) = self.tab_drag {
            if moved || (x - x0).abs() > 6.0 {
                self.tab_drag = Some((i, x0, true));
                // Swap with the neighbour once the pointer passes its middle.
                if let Some((r, _)) = self.tab_rects.get(i).copied() {
                    let target = if x < r.x && i > 0 {
                        Some(i - 1)
                    } else if x > r.right() && i + 1 < self.tabs.len() {
                        Some(i + 1)
                    } else {
                        None
                    };
                    if let Some(j) = target {
                        self.tabs.swap(i, j);
                        self.active = j;
                        self.tab_drag = Some((j, x, true));
                        self.layout();
                        self.invalidate();
                        self.session_dirty = true;
                    }
                }
            }
            return;
        }
        let h = self.hit(x, y);
        if h != self.hover {
            self.hover = h;
            self.invalidate();
        }
    }

    fn drag_to(&mut self, drag: DragMode, x: f32, y: f32) {
        match drag {
            DragMode::VScroll { grab } => {
                let geom = self.editor_geom();
                let vb = geom.vbar();
                let (_, shown) = self.with_view(|v, cx| v.scroll_fraction(cx));
                let thumb_h = (vb.h * shown).max(28.0).min(vb.h);
                let f = (y - grab as f32 - vb.y) / (vb.h - thumb_h).max(1.0);
                self.with_view(|v, cx| v.set_scroll_fraction(cx, f));
                self.invalidate();
            }
            DragMode::HScroll { grab } => {
                let geom = self.editor_geom();
                let hb = geom.hbar();
                let v = &mut self.tab_mut().view;
                let total = v.content_w + 40.0;
                let thumb_w = (hb.w * geom.text_w / total).max(28.0).min(hb.w);
                let max = (total - geom.text_w).max(0.0);
                let f = (x - grab as f32 - hb.x) / (hb.w - thumb_w).max(1.0);
                v.scroll_x = (f.clamp(0.0, 1.0) * max).round();
                self.invalidate();
            }
            _ => {
                let r = self.r_edit;
                if y < r.y || y > r.bottom() {
                    self.timer(TIMER_SCROLL, 40);
                } else {
                    self.kill_timer(TIMER_SCROLL);
                }
                self.extend_drag(drag, x, y);
            }
        }
    }

    fn extend_drag(&mut self, drag: DragMode, x: f32, y: f32) {
        let (pos, up) = self.with_view(|v, cx| v.pos_at(cx, x, y));
        let tab = self.tab_mut();
        let doc = &tab.doc;
        match drag {
            DragMode::Chars => {
                tab.view.sel.caret = pos;
                tab.view.upstream = up;
            }
            DragMode::Words(a, b) => {
                let (wa, wb) = doc.word_at(pos);
                tab.view.sel = if pos < a { Sel::new(b, wa) } else { Sel::new(a, wb.max(b)) };
            }
            DragMode::Lines(a, b) => {
                let ls = doc.line_start_of(pos);
                let le = doc.next_newline(pos).map(|p| p + 1).unwrap_or(doc.len());
                tab.view.sel = if pos < a { Sel::new(b, ls) } else { Sel::new(a, le.max(b)) };
            }
            _ => {}
        }
        self.invalidate();
    }

    pub fn on_mouse_up(&mut self, x: f32, y: f32, button: u8) {
        let hit = self.hit(x, y);
        let down = std::mem::replace(&mut self.down, Hit::None);
        unsafe {
            let _ = ReleaseCapture();
        }
        self.kill_timer(TIMER_SCROLL);
        if self.split_drag.take().is_some() {
            self.settings.save();
        }
        if !self.tabs.is_empty() {
            self.tab_mut().view.drag = None;
        }
        self.tab_drag = None;
        match (button, down, hit) {
            (0, Hit::TabClose(i), Hit::TabClose(j)) if i == j => self.pending.push(Deferred::Cmd(Cmd::CloseTabAt(i))),
            (0, Hit::NewTab, Hit::NewTab) => self.pending.push(Deferred::Cmd(Cmd::NewTab)),
            (0, Hit::ThemeToggle, Hit::ThemeToggle) => {
                let to = if self.theme.dark { ThemeMode::Light } else { ThemeMode::Dark };
                self.exec(Cmd::Theme(to));
                self.flash(if to == ThemeMode::Dark { "Dark theme" } else { "Light theme" }, false);
            }
            (0, Hit::Find(p), Hit::Find(q)) if p == q => self.find_part(p),
            (0, Hit::Notice(i), Hit::Notice(j)) if i == j => self.notice_action(i),
            (0, Hit::Status(a), Hit::Status(b)) if a == b => match a {
                StatusItem::Position => self.pending.push(Deferred::Cmd(Cmd::GoToLine)),
                StatusItem::Zoom => self.pending.push(Deferred::Cmd(Cmd::ZoomReset)),
                other => self.pending.push(Deferred::StatusMenu(other)),
            },
            (1, _, Hit::Text) | (1, _, Hit::Gutter) => self.pending.push(Deferred::ContextMenu(x, y)),
            (1, _, Hit::Tab(i)) => self.pending.push(Deferred::TabMenu(i, x, y)),
            (2, Hit::Tab(i), Hit::Tab(j)) if i == j => self.pending.push(Deferred::Cmd(Cmd::CloseTabAt(i))),
            _ => {}
        }
        self.invalidate();
    }

    pub fn on_mouse_leave(&mut self) {
        self.mouse_tracking = false;
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.invalidate();
        }
    }

    pub fn on_wheel(&mut self, delta: i32, horizontal: bool, x: f32, y: f32) {
        let m = mods();
        if m.ctrl && !horizontal {
            self.exec(if delta > 0 { Cmd::ZoomIn } else { Cmd::ZoomOut });
            return;
        }
        if self.r_struct.w > 0.0 && self.r_struct.contains(x, y) {
            let s = &mut self.tab_mut().structure;
            s.scroll = (s.scroll - delta as f32 / 120.0 * 3.0 * super::structure::ROW_H).max(0.0);
            self.invalidate();
            return;
        }
        if self.r_tabs.contains(x, y) {
            self.tab_scroll -= delta as f32 / 120.0 * 60.0;
            self.layout();
            self.invalidate();
            return;
        }
        if horizontal || m.shift {
            if !self.style.wrap {
                let geom = self.editor_geom();
                let v = &mut self.tab_mut().view;
                let max = (v.content_w + 40.0 - geom.text_w).max(0.0);
                let d = if horizontal { delta } else { -delta };
                v.scroll_x = (v.scroll_x + d as f32 / 120.0 * 80.0).clamp(0.0, max);
                self.invalidate();
            }
            return;
        }
        let mut lines = 3u32;
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some(&mut lines as *mut u32 as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let rows = if lines == u32::MAX {
            // "one screen at a time"
            let n = ((self.r_edit.h / self.style.row_h) as i64 - 1).max(1);
            if delta > 0 { -n } else { n }
        } else {
            let r = -(delta as i64) * lines.max(1) as i64 / 120;
            if r == 0 { -(delta.signum() as i64) } else { r }
        };
        self.with_view(|v, cx| v.scroll_rows(cx, rows));
        self.invalidate();
    }

    pub fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_CARET => {
                self.caret_on = !self.caret_on;
                self.invalidate();
            }
            TIMER_JOBS => self.poll_jobs(),
            TIMER_DISK => {
                self.check_disk();
                if self.session_dirty && self.settings.restore_session && self.last_session_save.elapsed() > Duration::from_secs(20) {
                    self.save_session();
                }
            }
            TIMER_SEARCH => self.start_count(),
            TIMER_SCROLL => {
                let Some(drag) = self.tabs.get(self.active).and_then(|t| t.view.drag) else {
                    self.kill_timer(TIMER_SCROLL);
                    return;
                };
                let mut pt = POINT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
                }
                let (x, y) = (self.px_to_dip(pt.x), self.px_to_dip(pt.y));
                let r = self.r_edit;
                let dist = if y < r.y { y - r.y } else if y > r.bottom() { y - r.bottom() } else { 0.0 };
                if dist != 0.0 {
                    let n = ((dist.abs() / 20.0).ceil() as i64).clamp(1, 20) * dist.signum() as i64;
                    self.with_view(|v, cx| {
                        v.scroll_rows(cx, n);
                        // so the selection reaches what is under the pointer now, not before the scroll
                        v.layout_rows(cx);
                    });
                    self.extend_drag(drag, x, y.max(r.y).min((r.bottom() - 1.0).max(r.y)));
                }
            }
            _ => {}
        }
    }

    /// Writes the session (when that's on). Returns false if unsaved work couldn't be written to it.
    pub fn save_session(&mut self) -> bool {
        self.last_session_save = Instant::now();
        self.session_dirty = false;
        !self.settings.restore_session || session::save(&mut self.tabs, self.active)
    }

    fn notice_action(&mut self, i: usize) {
        let Some(n) = self.tab().notice.as_ref() else { return };
        let Some((_, a)) = n.actions.get(i).cloned() else { return };
        match a {
            NoticeAction::Reload => {
                let i = self.active;
                self.reload(i, None);
            }
            NoticeAction::KeepMine => {
                let tab = self.tab_mut();
                tab.notice = None;
                tab.doc.mark_dirty();
            }
            NoticeAction::SaveAs => self.pending.push(Deferred::Cmd(Cmd::SaveAs)),
            NoticeAction::SaveUtf8 => {
                let tab = self.tab_mut();
                tab.doc.encoding = Encoding::Utf8;
                tab.doc.bom = false;
                tab.notice = None;
                self.pending.push(Deferred::Cmd(Cmd::Save));
            }
            NoticeAction::GoTo(off) => self.go_to(off, true),
            NoticeAction::Dismiss => self.tab_mut().notice = None,
        }
        self.layout();
        self.invalidate();
    }

    // ---- commands without dialogs ----

    pub fn exec(&mut self, cmd: Cmd) {
        if self.tabs.is_empty() {
            self.new_untitled();
        }
        match cmd {
            Cmd::NewTab => self.new_untitled(),
            Cmd::Undo | Cmd::Redo => {
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let r = if cmd == Cmd::Undo { tab.doc.undo() } else { tab.doc.redo() };
                if let Some(sel) = r {
                    tab.view.sel = sel;
                    self.after_edit();
                }
            }
            Cmd::Copy => {
                self.copy();
            }
            Cmd::Cut => self.cut(),
            Cmd::Paste => self.paste(),
            Cmd::Delete => {
                if self.editable() {
                    let tab = self.tab_mut();
                    tab.view.sel = editor::delete_forward(&mut tab.doc, tab.view.sel, false);
                    self.after_edit();
                }
            }
            Cmd::SelectAll => {
                let tab = self.tab_mut();
                tab.view.sel = Sel::new(0, tab.doc.len());
                self.invalidate();
            }
            Cmd::Find => self.open_find(BarMode::Find),
            Cmd::Replace => self.open_find(BarMode::Replace),
            Cmd::GoToLine => self.open_find(BarMode::GoTo),
            Cmd::FindNext => self.find_next(true),
            Cmd::FindPrev => self.find_next(false),
            Cmd::DuplicateLine | Cmd::DeleteLine | Cmd::MoveLineUp | Cmd::MoveLineDown | Cmd::Indent | Cmd::Outdent => {
                if !self.editable() {
                    return;
                }
                let unit = self.settings.indent_unit();
                let ts = self.settings.tab_size;
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                tab.doc.seal();
                let r = match cmd {
                    Cmd::DuplicateLine => Some(editor::duplicate(&mut tab.doc, sel)),
                    Cmd::DeleteLine => editor::delete_lines(&mut tab.doc, sel),
                    Cmd::MoveLineUp => editor::move_lines(&mut tab.doc, sel, false),
                    Cmd::MoveLineDown => editor::move_lines(&mut tab.doc, sel, true),
                    Cmd::Indent => editor::indent_lines(&mut tab.doc, sel, &unit, ts, false),
                    _ => editor::indent_lines(&mut tab.doc, sel, &unit, ts, true),
                };
                tab.doc.seal();
                match r {
                    Some(s) => {
                        tab.view.sel = s;
                        self.after_edit();
                    }
                    None => self.flash("Too many lines selected for that.", true),
                }
            }
            Cmd::InsertDateTime => {
                let t = now_text();
                self.tab_mut().doc.seal();
                self.type_text(&t);
                self.tab_mut().doc.seal();
            }
            Cmd::ToggleWrap => {
                self.settings.wrap = !self.settings.wrap;
                for t in &mut self.tabs {
                    t.view.scroll_x = 0.0;
                }
                self.settings_changed();
                let c = self.tab().view.sel.caret;
                self.with_view(|v, cx| v.reveal(cx, c, false));
            }
            Cmd::ToggleLineNumbers => {
                self.settings.line_numbers = !self.settings.line_numbers;
                self.settings_changed();
            }
            Cmd::ToggleStructure => {
                self.settings.structure_panel = !self.settings.structure_panel;
                if self.settings.structure_panel && self.tab().lang != Lang::Json {
                    self.flash("The structure panel shows JSON files (Format → Language → JSON).", false);
                }
                self.tab_mut().structure.mark_dirty();
                self.settings_changed();
            }
            Cmd::TogglePathBar => {
                self.settings.path_bar = !self.settings.path_bar;
                self.settings_changed();
            }
            Cmd::CopyJsonPath => {
                let notify = self.notify.clone();
                let tab = &mut self.tabs[self.active];
                let caret = tab.view.sel.caret;
                let p = tab.structure.path_at_caret(&mut tab.doc, caret, &notify);
                let busy = tab.structure.busy();
                match p {
                    Some(p) if !p.is_empty() => {
                        let s = crate::core::jsonnav::path_string(&p);
                        win::set_clipboard(self.hwnd, s.as_bytes());
                        self.flash(format!("Copied {s}"), false);
                    }
                    Some(_) => self.flash("No JSON path here", true),
                    None => self.flash("Still reading the JSON structure — try again in a moment.", true),
                }
                if busy {
                    self.timer(TIMER_JOBS, 100);
                }
            }
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                let z = self.settings.zoom;
                self.settings.zoom = match cmd {
                    Cmd::ZoomIn => ZOOM_STEPS.iter().copied().find(|&s| s > z + 0.001).unwrap_or(5.0),
                    Cmd::ZoomOut => ZOOM_STEPS.iter().rev().copied().find(|&s| s < z - 0.001).unwrap_or(0.5),
                    _ => 1.0,
                };
                self.settings_changed();
                self.flash(format!("Zoom {:.0}%", self.settings.zoom * 100.0), false);
            }
            Cmd::Font(i) => {
                if let Some(f) = self.mono_fonts.as_ref().and_then(|v| v.get(i as usize)).cloned() {
                    self.settings.font = f;
                    self.settings_changed();
                }
            }
            Cmd::FontSize(n) => {
                self.settings.font_size = n as f32;
                self.settings_changed();
            }
            Cmd::Theme(t) => {
                self.settings.theme = t;
                self.settings.save();
                self.apply_theme();
            }
            Cmd::Format | Cmd::Minify | Cmd::Validate => {
                let f = match self.tab().lang {
                    Lang::Json => Fmt::Json,
                    Lang::Xml => Fmt::Xml,
                    _ => {
                        self.flash("Formatting works for JSON and XML files (the language is in the status bar).", true);
                        return;
                    }
                };
                self.start_task(match cmd {
                    Cmd::Format => TaskKind::Format(f),
                    Cmd::Minify => TaskKind::Minify(f),
                    _ => TaskKind::Validate(f),
                });
            }
            Cmd::ToggleComment => {
                let Some(style) = self.tab().lang.comment() else {
                    self.flash(format!("{} has no comments", self.tab().lang.label()), true);
                    return;
                };
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                tab.doc.seal();
                let r = editor::toggle_comment(&mut tab.doc, sel, style);
                tab.doc.seal();
                match r {
                    Some(s) => {
                        tab.view.sel = s;
                        self.after_edit();
                    }
                    None => self.flash("Too many lines selected for that.", true),
                }
            }
            Cmd::Lines(op) => {
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                if sel.is_empty() || (sel.start() == 0 && sel.end() == tab.doc.len()) {
                    // nothing (or everything) selected: the whole document
                    self.start_task(TaskKind::Lines(op));
                    return;
                }
                // the lines the selection touches, without the last line break
                let a = tab.doc.line_start_of(sel.start());
                let last = if sel.end() > a && tab.doc.line_start_of(sel.end()) == sel.end() { sel.end() - 1 } else { sel.end() };
                let b = tab.doc.line_end_of(last).max(a);
                if b - a > SELECTION_MAX {
                    self.flash("Select less text (up to 16 MB), or select nothing to do the whole file.", true);
                    return;
                }
                let text = tab.doc.read(a, b);
                let (out, count) = lines::apply(op, &text);
                if count == 0 && !matches!(op, LineOp::SortAsc | LineOp::SortDesc) {
                    self.flash(nothing_to_clean(op), false);
                    return;
                }
                let new = editor::replace_range(&mut tab.doc, sel, a, b, &out);
                tab.view.sel = new;
                self.after_edit();
                self.flash(lines_done(op, count), false);
            }
            Cmd::Case(op) => {
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let mut sel = tab.view.sel;
                if sel.is_empty() {
                    // the word at the caret
                    let (wa, wb) = tab.doc.word_at(sel.caret);
                    if wa == wb {
                        return;
                    }
                    sel = Sel::new(wa, wb);
                }
                let (a, b) = (sel.start(), sel.end());
                if b - a > SELECTION_MAX {
                    self.flash("Select less text (up to 16 MB) to change its case.", true);
                    return;
                }
                let text = tab.doc.read(a, b);
                let out = lines::change_case(&text, op);
                if out == text {
                    return;
                }
                let new = editor::replace_range(&mut tab.doc, tab.view.sel, a, b, &out);
                // keep the selection's direction
                tab.view.sel = if sel.anchor > sel.caret { Sel::new(new.end(), new.start()) } else { new };
                self.after_edit();
            }
            Cmd::SetEol(e) => {
                if self.tab().doc.eol == e && !self.tab().doc.is_empty() {
                    // still convert stray line endings
                }
                if self.tab().doc.is_empty() {
                    self.tab_mut().doc.eol = e;
                    self.invalidate();
                } else {
                    self.start_task(TaskKind::Eol(e));
                }
            }
            Cmd::SaveEncoding(e) => {
                let tab = self.tab_mut();
                if tab.doc.encoding != e {
                    tab.doc.encoding = e;
                    tab.doc.bom = matches!(e, Encoding::Utf8Bom | Encoding::Utf16Le | Encoding::Utf16Be);
                    tab.doc.mark_dirty();
                    self.update_title();
                    self.flash(format!("Will be saved as {}", e.label()), false);
                }
            }
            Cmd::ReopenEncoding(e) => {
                let i = self.active;
                self.reload(i, Some(e));
            }
            Cmd::SetLang(l) => {
                let tab = self.tab_mut();
                tab.lang = l;
                tab.view.clear_cache();
                self.session_dirty = true;
                self.invalidate();
            }
            Cmd::IndentSpaces(b) => {
                self.settings.use_spaces = b;
                self.settings_changed();
            }
            Cmd::TabSize(n) => {
                self.settings.tab_size = n;
                self.settings_changed();
            }
            Cmd::NextTab | Cmd::PrevTab => {
                let n = self.tabs.len();
                let i = if cmd == Cmd::NextTab { (self.active + 1) % n } else { (self.active + n - 1) % n };
                self.activate(i);
            }
            Cmd::ActivateTab(i) => {
                let i = if i == 8 { self.tabs.len() - 1 } else { i };
                self.activate(i.min(self.tabs.len() - 1));
            }
            Cmd::Reload => {
                let i = self.active;
                self.reload(i, None);
            }
            Cmd::RevealFile => {
                if let Some(p) = self.tab().doc.path.clone() {
                    win::reveal_in_explorer(&p);
                }
            }
            Cmd::CopyPath => {
                if let Some(p) = self.tab().doc.path.clone() {
                    win::set_clipboard(self.hwnd, p.to_string_lossy().as_bytes());
                    self.flash("Path copied", false);
                }
            }
            Cmd::OpenRecent(i) => {
                if let Some(p) = self.settings.recent.get(i).cloned() {
                    self.open_paths(&[p]);
                }
            }
            Cmd::ClearRecent => {
                self.settings.recent.clear();
                self.settings.save();
            }
            Cmd::OpenDataFolder => {
                let d = data_dir();
                let _ = std::fs::create_dir_all(&d);
                unsafe {
                    ShellExecuteW(self.hwnd, w!("open"), &HSTRING::from(d.as_os_str()), None, None, SW_SHOWNORMAL);
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    fn settings_changed(&mut self) {
        self.settings.save();
        self.rebuild_style();
        self.layout();
    }

    // ---- JSON structure ----

    /// Moves to a JSON value: selects it if it's a small scalar, otherwise puts the caret at its start.
    fn jump_to_value(&mut self, start: u64, end: u64, container: bool) {
        let tab = self.tab_mut();
        let len = tab.doc.len();
        let (start, end) = (start.min(len), end.min(len));
        tab.view.sel = if !container && end > start && end - start <= 64 * 1024 { Sel::new(start, end) } else { Sel::at(start) };
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.doc.seal();
        self.with_view(|v, cx| v.reveal(cx, start, true));
        self.restart_caret();
        unsafe {
            let _ = SetFocus(self.hwnd);
        }
        self.invalidate();
    }

    fn jump_path(&mut self, i: usize) {
        let step = self.tab().structure.path.as_ref().and_then(|p| p.get(i)).cloned();
        if let Some(st) = step {
            let first = self.tab().doc.byte_at(st.start).unwrap_or(0);
            self.jump_to_value(st.start, st.end, matches!(first, b'{' | b'['));
        }
    }

    fn struct_click(&mut self, i: usize, chevron: bool, double: bool) {
        let Some(row) = self.tab().structure.rows.get(i).cloned() else { return };
        if row.expandable && (chevron || double) {
            self.tab_mut().structure.toggle(row.key);
        }
        if !chevron && row.end > row.start {
            self.tab_mut().structure.selected = Some(row.key);
            let container = row.kind.is_none_or(|k| k.is_container());
            self.jump_to_value(row.start, row.end, container);
        }
        self.invalidate();
    }

    // ---- menus ----

    pub fn menu_items(&mut self, idx: usize) -> Vec<Item> {
        let tab = self.tab();
        let has_path = tab.doc.path.is_some();
        let sel = !tab.view.sel.is_empty();
        let s = &self.settings;
        match idx {
            0 => {
                let mut recent: Vec<Item> = s
                    .recent
                    .iter()
                    .enumerate()
                    .map(|(i, p)| item(Cmd::OpenRecent(i), &p.display().to_string().replace('&', "&&"), ""))
                    .collect();
                if recent.is_empty() {
                    recent.push(enabled(Cmd::ClearRecent, "No recent files", "", false));
                } else {
                    recent.push(Item::Sep);
                    recent.push(item(Cmd::ClearRecent, "Clear list", ""));
                }
                vec![
                    item(Cmd::NewTab, "&New tab", "Ctrl+N"),
                    item(Cmd::Open, "&Open…", "Ctrl+O"),
                    sub("Open &recent", recent),
                    Item::Sep,
                    item(Cmd::Save, "&Save", "Ctrl+S"),
                    item(Cmd::SaveAs, "Save &as…", "Ctrl+Shift+S"),
                    item(Cmd::SaveAll, "Save a&ll", "Ctrl+Alt+S"),
                    Item::Sep,
                    enabled(Cmd::Reload, "Re&load from disk", "", has_path),
                    enabled(Cmd::RevealFile, "Show in &folder", "", has_path),
                    enabled(Cmd::CopyPath, "Copy file &path", "", has_path),
                    Item::Sep,
                    item(Cmd::CloseTab, "&Close tab", "Ctrl+W"),
                    item(Cmd::CloseOthers, "Close &other tabs", ""),
                    item(Cmd::Exit, "E&xit", "Alt+F4"),
                ]
            }
            1 => vec![
                enabled(Cmd::Undo, "&Undo", "Ctrl+Z", tab.doc.can_undo()),
                enabled(Cmd::Redo, "&Redo", "Ctrl+Y", tab.doc.can_redo()),
                Item::Sep,
                item(Cmd::Cut, "Cu&t", "Ctrl+X"),
                item(Cmd::Copy, "&Copy", "Ctrl+C"),
                enabled(Cmd::Paste, "&Paste", "Ctrl+V", win::clipboard_has_text()),
                enabled(Cmd::Delete, "De&lete", "Del", sel),
                Item::Sep,
                item(Cmd::Find, "&Find…", "Ctrl+F"),
                item(Cmd::FindNext, "Find &next", "F3"),
                item(Cmd::FindPrev, "Find pre&vious", "Shift+F3"),
                item(Cmd::Replace, "R&eplace…", "Ctrl+H"),
                item(Cmd::GoToLine, "&Go to line…", "Ctrl+G"),
                Item::Sep,
                item(Cmd::SelectAll, "Select &all", "Ctrl+A"),
                Item::Sep,
                item(Cmd::DuplicateLine, "&Duplicate line", "Ctrl+D"),
                item(Cmd::DeleteLine, "Delete l&ine", "Ctrl+Shift+K"),
                item(Cmd::MoveLineUp, "Move line u&p", "Alt+Up"),
                item(Cmd::MoveLineDown, "Move line do&wn", "Alt+Down"),
                enabled(Cmd::ToggleComment, "Toggle co&mment", "Ctrl+/", tab.lang.comment().is_some()),
                sub(
                    "Line&s",
                    vec![
                        item(Cmd::Lines(LineOp::SortAsc), "Sort &A to Z", ""),
                        item(Cmd::Lines(LineOp::SortDesc), "Sort &Z to A", ""),
                        Item::Sep,
                        item(Cmd::Lines(LineOp::Dedupe), "Remove &duplicate lines", ""),
                        item(Cmd::Lines(LineOp::RemoveBlank), "Remove &blank lines", ""),
                        item(Cmd::Lines(LineOp::TrimTrailing), "&Trim spaces at line ends", ""),
                    ],
                ),
                sub(
                    "C&hange case",
                    vec![
                        item(Cmd::Case(CaseOp::Upper), "&UPPERCASE", "Ctrl+Shift+U"),
                        item(Cmd::Case(CaseOp::Lower), "&lowercase", "Ctrl+U"),
                        item(Cmd::Case(CaseOp::Title), "&Title Case", ""),
                    ],
                ),
                Item::Sep,
                item(Cmd::InsertDateTime, "Time/&date", "F5"),
            ],
            2 => {
                let fonts = self.monospace_fonts();
                let mut font_items: Vec<Item> = fonts
                    .iter()
                    .enumerate()
                    .map(|(i, f)| check(Cmd::Font(i as u16), f, "", *f == self.settings.font))
                    .collect();
                font_items.push(Item::Sep);
                for size in [9u8, 10, 11, 12, 14, 16, 18, 20, 24] {
                    font_items.push(check(
                        Cmd::FontSize(size),
                        &format!("{size} pt"),
                        "",
                        (self.settings.font_size - size as f32).abs() < 0.01,
                    ));
                }
                let s = &self.settings;
                let json = self.tab().lang == Lang::Json;
                let mut v = vec![
                    check(Cmd::ToggleWrap, "&Word wrap", "Alt+Z", s.wrap),
                    check(Cmd::ToggleLineNumbers, "&Line numbers", "", s.line_numbers),
                ];
                if json {
                    v.push(Item::Sep);
                    v.push(check(Cmd::ToggleStructure, "JSON &structure panel", "Ctrl+Shift+O", s.structure_panel));
                    v.push(check(Cmd::TogglePathBar, "JSON &path bar", "", s.path_bar));
                }
                v.extend([
                    Item::Sep,
                    sub(
                        "&Zoom",
                        vec![
                            item(Cmd::ZoomIn, "Zoom &in", "Ctrl+Plus"),
                            item(Cmd::ZoomOut, "Zoom &out", "Ctrl+Minus"),
                            item(Cmd::ZoomReset, "&Reset zoom", "Ctrl+0"),
                        ],
                    ),
                    sub("&Font", font_items),
                    sub(
                        "&Theme",
                        vec![
                            check(Cmd::Theme(ThemeMode::System), "Use &system setting", "", s.theme == ThemeMode::System),
                            check(Cmd::Theme(ThemeMode::Light), "&Light", "", s.theme == ThemeMode::Light),
                            check(Cmd::Theme(ThemeMode::Dark), "&Dark", "", s.theme == ThemeMode::Dark),
                        ],
                    ),
                ]);
                v
            }
            3 => {
                let tab = self.tab();
                let doc = &tab.doc;
                let s = &self.settings;
                let mut v = Vec::new();
                let f = match tab.lang {
                    Lang::Json => Some("JSON"),
                    Lang::Xml => Some("XML"),
                    _ => None,
                };
                if let Some(f) = f {
                    v.push(item(Cmd::Format, &format!("&Format {f}"), "Shift+Alt+F"));
                    v.push(item(Cmd::Minify, &format!("&Minify {f}"), ""));
                    v.push(item(Cmd::Validate, &format!("&Check {f}"), ""));
                    v.push(Item::Sep);
                }
                v.extend([
                    sub("&Language", self.lang_items()),
                    sub(
                        "Line &endings",
                        vec![
                            check(Cmd::SetEol(Eol::Crlf), "&Windows (CRLF)", "", doc.eol == Eol::Crlf),
                            check(Cmd::SetEol(Eol::Lf), "&Unix (LF)", "", doc.eol == Eol::Lf),
                        ],
                    ),
                    sub("En&coding", self.encoding_items()),
                    sub(
                        "&Indentation",
                        vec![
                            check(Cmd::IndentSpaces(true), "&Spaces", "", s.use_spaces),
                            check(Cmd::IndentSpaces(false), "&Tabs", "", !s.use_spaces),
                            Item::Sep,
                            check(Cmd::TabSize(2), "Width &2", "", s.tab_size == 2),
                            check(Cmd::TabSize(4), "Width &4", "", s.tab_size == 4),
                            check(Cmd::TabSize(8), "Width &8", "", s.tab_size == 8),
                        ],
                    ),
                ]);
                v
            }
            _ => vec![
                item(Cmd::Shortcuts, "&Keyboard shortcuts", ""),
                item(Cmd::MakeDefault, "Open files with Slate…", ""),
                item(Cmd::OpenDataFolder, "Open settings &folder", ""),
                Item::Sep,
                item(Cmd::About, "&About Slate", ""),
            ],
        }
    }

    pub fn lang_items(&self) -> Vec<Item> {
        let cur = self.tab().lang;
        let mut v = Vec::new();
        for (k, &l) in Lang::ALL.iter().enumerate() {
            if k > 0 && k % 16 == 0 {
                v.push(Item::ColBreak);
            }
            v.push(check(Cmd::SetLang(l), l.label(), "", l == cur));
        }
        v
    }

    pub fn encoding_items(&self) -> Vec<Item> {
        let doc = &self.tab().doc;
        let all = [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Ansi];
        let mut v: Vec<Item> = vec![enabled(Cmd::SaveEncoding(Encoding::Utf8), "Save with:", "", false)];
        for e in all {
            v.push(check(Cmd::SaveEncoding(e), &format!("   {}", e.label()), "", doc.encoding == e));
        }
        if doc.path.is_some() {
            v.push(Item::Sep);
            v.push(enabled(Cmd::ReopenEncoding(Encoding::Utf8), "Reopen as:", "", false));
            for e in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Ansi] {
                v.push(item(Cmd::ReopenEncoding(e), &format!("   {}", e.label()), ""));
            }
        }
        v
    }

    fn monospace_fonts(&mut self) -> Vec<String> {
        if self.mono_fonts.is_none() {
            let all = super::gfx::font_families(&self.g.dw);
            let mut v: Vec<String> = all.into_iter().filter(|(_, m)| *m).map(|(n, _)| n).collect();
            v.retain(|n| !n.starts_with('@'));
            if !v.contains(&self.settings.font) {
                v.push(self.settings.font.clone());
            }
            self.mono_fonts = Some(v);
        }
        self.mono_fonts.clone().unwrap()
    }
}

/// Whole documents up to this size can have their lines sorted or cleaned up (it all happens in memory).
const LINES_MAX: u64 = 512 << 20;
/// Selections up to this size can have their lines or letter case changed right away.
const SELECTION_MAX: u64 = 16 << 20;

fn lines_done(op: LineOp, n: u64) -> String {
    match op {
        LineOp::SortAsc | LineOp::SortDesc => format!("Sorted {}", plural(n, "line", "lines")),
        LineOp::Dedupe => format!("Removed {}", plural(n, "duplicate line", "duplicate lines")),
        LineOp::RemoveBlank => format!("Removed {}", plural(n, "blank line", "blank lines")),
        LineOp::TrimTrailing => format!("Trimmed spaces from {}", plural(n, "line", "lines")),
    }
}

fn nothing_to_clean(op: LineOp) -> &'static str {
    match op {
        LineOp::Dedupe => "No duplicate lines",
        LineOp::RemoveBlank => "No blank lines",
        _ => "No spaces at line ends",
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{} {many}", group(n)) }
}

// ---------------------------------------------------------------------------------------------------------------
// Things that may open modal UI: run outside the App borrow.

thread_local! {
    static MENU_SWITCH: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static MENU_BAR: std::cell::RefCell<(Vec<windows::Win32::Foundation::RECT>, usize, bool, bool)> =
        const { std::cell::RefCell::new((Vec::new(), 0, false, false)) };
}

/// Message filter during menu tracking: Left/Right and hovering move between the menu bar's menus.
unsafe extern "system" fn menu_hook(code: i32, wp: windows::Win32::Foundation::WPARAM, lp: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, EndMenu, HHOOK, MSG, MSGF_MENU, WM_KEYDOWN, WM_MOUSEMOVE};
    if code == MSGF_MENU as i32 {
        let msg = unsafe { &*(lp.0 as *const MSG) };
        let switch = MENU_BAR.with(|b| {
            let b = b.borrow();
            let (rects, cur, sel_popup, in_sub) = (&b.0, b.1, b.2, b.3);
            let n = rects.len();
            if n == 0 {
                return None;
            }
            if msg.message == WM_KEYDOWN {
                let k = VIRTUAL_KEY(msg.wParam.0 as u16);
                if k == VK_RIGHT && !sel_popup {
                    return Some((cur + 1) % n);
                }
                if k == VK_LEFT && !in_sub {
                    return Some((cur + n - 1) % n);
                }
            } else if msg.message == WM_MOUSEMOVE {
                let mut pt = POINT::default();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                }
                for (i, r) in rects.iter().enumerate() {
                    if i != cur && pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom {
                        return Some(i);
                    }
                }
            }
            None
        });
        if let Some(i) = switch {
            MENU_SWITCH.with(|s| s.set(Some(i)));
            unsafe {
                let _ = EndMenu();
            }
            return windows::Win32::Foundation::LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(HHOOK::default(), code, wp, lp) }
}

/// Tracks the menu selection (WM_MENUSELECT) so Left/Right know whether a submenu is involved.
pub fn on_menu_select(flags: u32, hmenu: isize, top: isize) {
    const MF_POPUP: u32 = 0x10;
    MENU_BAR.with(|b| {
        let mut b = b.borrow_mut();
        b.2 = flags & MF_POPUP != 0 && flags != 0xFFFF;
        b.3 = hmenu != top;
    });
}

thread_local! {
    pub static TOP_MENU: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

fn client_to_screen(hwnd: HWND, x: i32, y: i32) -> POINT {
    let mut p = POINT { x, y };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut p);
    }
    p
}

/// Shows a popup menu at client DIPs (x, y); returns the chosen command.
fn popup(cell: &Cell, items: Vec<Item>, x: f32, y: f32) -> Option<Cmd> {
    let (hwnd, px, py) = {
        let a = cell.borrow();
        (a.hwnd, a.dip_to_px(x), a.dip_to_px(y))
    };
    let mut ids = Vec::new();
    let menu = build_menu(&items, &mut ids);
    let p = client_to_screen(hwnd, px, py);
    TOP_MENU.with(|t| t.set(menu.0 as isize));
    let id = unsafe { TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN).0, p.x, p.y, hwnd, None) };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    let id = id.0 as usize;
    if id >= 1 && id <= ids.len() { Some(ids[id - 1]) } else { None }
}

fn show_menu_bar(cell: &Cell, mut idx: usize) {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, UnhookWindowsHookEx, WH_MSGFILTER};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    loop {
        let (items, rect, rects_screen, hwnd) = {
            let mut a = cell.borrow_mut();
            a.menu_open = Some(idx);
            a.hover = Hit::None;
            a.invalidate();
            let items = a.menu_items(idx);
            let r = a.menu_rects[idx];
            let hwnd = a.hwnd;
            let rects: Vec<windows::Win32::Foundation::RECT> = a
                .menu_rects
                .iter()
                .map(|r| {
                    let tl = client_to_screen(hwnd, a.dip_to_px(r.x), a.dip_to_px(r.y));
                    let br = client_to_screen(hwnd, a.dip_to_px(r.right()), a.dip_to_px(r.bottom()));
                    windows::Win32::Foundation::RECT { left: tl.x, top: tl.y, right: br.x, bottom: br.y }
                })
                .collect();
            (items, r, rects, hwnd)
        };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
        }
        MENU_BAR.with(|b| *b.borrow_mut() = (rects_screen, idx, false, false));
        MENU_SWITCH.with(|s| s.set(None));
        let hook = unsafe { SetWindowsHookExW(WH_MSGFILTER, Some(menu_hook), None, GetCurrentThreadId()) }.ok();
        let chosen = popup(cell, items, rect.x, rect.bottom());
        if let Some(h) = hook {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
        MENU_BAR.with(|b| b.borrow_mut().0.clear());
        cell.borrow_mut().menu_open = None;
        cell.borrow().invalidate();
        if let Some(next) = MENU_SWITCH.with(|s| s.take()) {
            idx = next;
            continue;
        }
        if let Some(cmd) = chosen {
            run_cmd(cell, cmd);
        }
        break;
    }
}

pub fn run(cell: &Cell, d: Deferred) {
    match d {
        Deferred::Cmd(c) => run_cmd(cell, c),
        Deferred::Menu(i) => show_menu_bar(cell, i),
        Deferred::ContextMenu(x, y) => {
            let items = {
                let a = cell.borrow();
                let tab = a.tab();
                let sel = !tab.view.sel.is_empty();
                let mut v = vec![
                    enabled(Cmd::Undo, "&Undo", "Ctrl+Z", tab.doc.can_undo()),
                    enabled(Cmd::Redo, "&Redo", "Ctrl+Y", tab.doc.can_redo()),
                    Item::Sep,
                    item(Cmd::Cut, "Cu&t", "Ctrl+X"),
                    item(Cmd::Copy, "&Copy", "Ctrl+C"),
                    enabled(Cmd::Paste, "&Paste", "Ctrl+V", win::clipboard_has_text()),
                    enabled(Cmd::Delete, "&Delete", "Del", sel),
                    Item::Sep,
                    item(Cmd::SelectAll, "Select &all", "Ctrl+A"),
                ];
                if tab.lang.comment().is_some() {
                    v.push(Item::Sep);
                    v.push(item(Cmd::ToggleComment, "Toggle co&mment", "Ctrl+/"));
                }
                if let Some(f) = match tab.lang {
                    Lang::Json => Some("JSON"),
                    Lang::Xml => Some("XML"),
                    _ => None,
                } {
                    v.push(Item::Sep);
                    v.push(item(Cmd::Format, &format!("&Format {f}"), "Shift+Alt+F"));
                    v.push(item(Cmd::Validate, &format!("Chec&k {f}"), ""));
                }
                if tab.lang == Lang::Json {
                    v.push(item(Cmd::CopyJsonPath, "Copy JSON pat&h", ""));
                }
                v
            };
            if let Some(c) = popup(cell, items, x, y) {
                run_cmd(cell, c);
            }
        }
        Deferred::TabMenu(i, x, y) => {
            let Some(id) = cell.borrow().tabs.get(i).map(|t| t.id) else { return };
            let has_path = cell.borrow().tabs[i].doc.path.is_some();
            let items = vec![
                item(Cmd::CloseTab, "&Close", "Ctrl+W"),
                item(Cmd::CloseOthers, "Close &others", ""),
                item(Cmd::CloseRight, "Close tabs to the &right", ""),
                Item::Sep,
                enabled(Cmd::CopyPath, "Copy &path", "", has_path),
                enabled(Cmd::RevealFile, "Show in &folder", "", has_path),
            ];
            cell.borrow_mut().activate(i);
            if let Some(c) = popup(cell, items, x, y) {
                // Other things can happen while the menu is open: only act if that tab is still the active one.
                if cell.borrow().tab().id == id {
                    run_cmd(cell, c);
                }
            }
        }
        Deferred::StatusMenu(item_kind) => {
            let (items, rect) = {
                let a = cell.borrow();
                let rect = a.status_rects.iter().find(|(k, _)| *k == item_kind).map(|(_, r)| *r).unwrap_or_default();
                let items = match item_kind {
                    StatusItem::Lang => a.lang_items(),
                    StatusItem::Encoding => a.encoding_items(),
                    StatusItem::Eol => {
                        let e = a.tab().doc.eol;
                        vec![
                            check(Cmd::SetEol(Eol::Crlf), "&Windows (CRLF)", "", e == Eol::Crlf),
                            check(Cmd::SetEol(Eol::Lf), "&Unix (LF)", "", e == Eol::Lf),
                        ]
                    }
                    _ => Vec::new(),
                };
                (items, rect)
            };
            if items.is_empty() {
                return;
            }
            // Open upwards from the status bar.
            let n = items.len() as f32;
            if let Some(c) = popup(cell, items, rect.x, (rect.y - n * 24.0 - 8.0).max(0.0)) {
                run_cmd(cell, c);
            }
        }
    }
}

pub fn run_cmd(cell: &Cell, cmd: Cmd) {
    match cmd {
        Cmd::Open => {
            let (hwnd, dir) = {
                let a = cell.borrow();
                (a.hwnd, a.tab().doc.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf))
            };
            let paths = win::open_dialog(hwnd, dir.as_deref());
            if !paths.is_empty() {
                cell.borrow_mut().open_paths(&paths);
            }
        }
        Cmd::Save => {
            let i = cell.borrow().active;
            save_tab(cell, i, false, false);
        }
        Cmd::SaveAs => {
            let i = cell.borrow().active;
            save_tab(cell, i, true, false);
        }
        Cmd::SaveAll => {
            let ids: Vec<u64> = cell.borrow().tabs.iter().filter(|t| t.doc.is_dirty()).map(|t| t.id).collect();
            for id in ids {
                let Some(i) = tab_index(cell, id) else { continue };
                if !cell.borrow().tabs[i].doc.is_dirty() {
                    continue;
                }
                cell.borrow_mut().activate(i);
                if !save_tab(cell, i, false, false) {
                    break;
                }
            }
        }
        Cmd::CloseTab => {
            let i = cell.borrow().active;
            close_tab(cell, i);
        }
        Cmd::CloseTabAt(i) => {
            close_tab(cell, i);
        }
        Cmd::CloseOthers | Cmd::CloseRight => {
            let (keep_id, active) = {
                let a = cell.borrow();
                (a.tab().id, a.active)
            };
            let ids: Vec<u64> = {
                let a = cell.borrow();
                a.tabs
                    .iter()
                    .enumerate()
                    .filter(|(i, t)| t.id != keep_id && (cmd == Cmd::CloseOthers || *i > active))
                    .map(|(_, t)| t.id)
                    .collect()
            };
            for id in ids {
                let pos = cell.borrow().tabs.iter().position(|t| t.id == id);
                if let Some(i) = pos {
                    if !close_tab(cell, i) {
                        break;
                    }
                }
            }
        }
        Cmd::Exit => close_window(cell),
        Cmd::Copy | Cmd::Cut => {
            let (size, hwnd) = {
                let a = cell.borrow();
                (a.copy_size(), a.hwnd)
            };
            if size > BIG_CLIPBOARD {
                let q = format!("Copy {} to the clipboard?", format_size(size));
                let detail = "That much text needs a lot of memory and can make other programs slow when they paste it.";
                if win::ask(hwnd, "Slate", &q, detail, &["Copy", "Cancel"]) != Some(0) {
                    return;
                }
            }
            cell.borrow_mut().exec(cmd);
        }
        Cmd::ReopenEncoding(_) | Cmd::Reload => {
            let (dirty, hwnd, title) = {
                let a = cell.borrow();
                (a.tab().doc.is_dirty(), a.hwnd, a.tab().title())
            };
            if dirty {
                let q = format!("Reload {title} and lose your changes?");
                if win::ask(hwnd, "Slate", &q, "", &["Reload", "Cancel"]) != Some(0) {
                    return;
                }
            }
            cell.borrow_mut().exec(cmd);
        }
        Cmd::About => {
            let hwnd = cell.borrow().hwnd;
            let text = format!(
                "Slate {}\n\nA fast, simple text editor that opens files of any size.\n\nSettings and unsaved work are kept in\n{}",
                env!("CARGO_PKG_VERSION"),
                data_dir().display()
            );
            win::info(hwnd, "About Slate", &text);
        }
        Cmd::Shortcuts => {
            let hwnd = cell.borrow().hwnd;
            win::info(hwnd, "Keyboard shortcuts", SHORTCUTS);
        }
        Cmd::MakeDefault => super::install::make_default(cell),
        other => {
            let mut a = cell.borrow_mut();
            a.exec(other);
        }
    }
}

fn tab_index(cell: &Cell, id: u64) -> Option<usize> {
    cell.borrow().tabs.iter().position(|t| t.id == id)
}

/// Saves tab `i` (asking for a name when needed). Returns false if the user cancelled or it can't be saved now.
pub fn save_tab(cell: &Cell, i: usize, ask_name: bool, close_after: bool) -> bool {
    let (id, path, title, hwnd) = {
        let a = cell.borrow();
        let Some(t) = a.tabs.get(i) else { return false };
        (t.id, t.doc.path.clone(), t.title(), a.hwnd)
    };
    let path = match path {
        Some(p) if !ask_name => p,
        other => {
            let name = if other.is_some() { title.clone() } else { format!("{title}.txt") };
            let dir = other.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf);
            match win::save_dialog(hwnd, &name, dir.as_deref()) {
                Some(p) => p,
                None => return false,
            }
        }
    };
    // The dialog let other things happen (tabs can close or move meanwhile): find the tab again.
    let Some(i) = tab_index(cell, id) else { return false };
    let mut a = cell.borrow_mut();
    let enc = a.tabs[i].doc.encoding;
    a.start_save(i, path, enc, close_after)
}

/// Closes tab `i`, asking about unsaved changes. Returns false if the user cancelled.
pub fn close_tab(cell: &Cell, i: usize) -> bool {
    let (id, dirty, saving_same, title, hwnd) = {
        let a = cell.borrow();
        let Some(t) = a.tabs.get(i) else { return true };
        let dirty = t.doc.is_dirty() && !(t.doc.is_empty() && t.doc.path.is_none());
        (t.id, dirty, t.save.as_ref().is_some_and(|s| s.version == t.doc.version), t.title(), a.hwnd)
    };
    if saving_same {
        // Being saved, nothing changed since: close once that's done.
        let mut a = cell.borrow_mut();
        if let Some(st) = a.tabs[i].save.as_mut() {
            st.close_after = true;
        }
        a.flash(format!("{title} will close once it's saved."), false);
        return true;
    }
    if dirty {
        cell.borrow_mut().activate(i);
        let q = format!("Do you want to save changes to {title}?");
        let choice = win::ask(hwnd, "Slate", &q, "", &["Save", "Don't save", "Cancel"]);
        // The dialog let other things happen (tabs can close or move meanwhile): find the tab again.
        let Some(i) = tab_index(cell, id) else { return true };
        match choice {
            Some(0) => return save_tab(cell, i, false, true),
            Some(1) => {
                let mut a = cell.borrow_mut();
                let t = &mut a.tabs[i];
                if let Some(st) = t.save.as_mut() {
                    // An earlier save is still running: let it finish, then close without the newer changes.
                    st.close_after = true;
                    t.discard = true;
                    return true;
                }
            }
            _ => return false,
        }
    }
    cell.borrow_mut().remove_tab(i);
    true
}

/// Closes the window: unsaved work is kept in the session (or asked about when it can't be).
pub fn close_window(cell: &Cell) {
    // Keep what can be kept first; whatever that doesn't cover is asked about.
    let session_ok = cell.borrow_mut().save_session();
    let restore = cell.borrow().settings.restore_session;
    let ids = cell.borrow().unkept_tabs(session_ok);
    let mut saving = false;
    for id in ids {
        let Some(i) = tab_index(cell, id) else { continue };
        let (title, hwnd) = {
            let a = cell.borrow();
            (a.tabs[i].title(), a.hwnd)
        };
        cell.borrow_mut().activate(i);
        let detail = if !restore {
            ""
        } else if !session_ok {
            "Slate couldn't keep unsaved changes for next time (the settings folder can't be written)."
        } else {
            "This file is too big to keep unsaved changes for next time."
        };
        let q = format!("Do you want to save changes to {title}?");
        let choice = win::ask(hwnd, "Slate", &q, detail, &["Save", "Don't save", "Cancel"]);
        let Some(i) = tab_index(cell, id) else { continue };
        match choice {
            Some(0) => {
                if !save_tab(cell, i, false, false) {
                    cell.borrow_mut().cancel_close();
                    return;
                }
                saving = true;
            }
            Some(1) => cell.borrow_mut().tabs[i].discard = true,
            _ => {
                cell.borrow_mut().cancel_close();
                return;
            }
        }
    }
    let hwnd = {
        let mut a = cell.borrow_mut();
        if saving || a.tabs.iter().any(|t| t.save.is_some()) {
            // Close once the saves are done (poll_jobs comes back here).
            a.closing = true;
            if saving {
                a.flash("Saving before closing…", false);
            }
            return;
        }
        a.save_window_placement();
        if restore {
            a.save_session();
        } else {
            session::clear();
        }
        a.settings.save();
        a.hwnd
    };
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

impl App {
    pub fn save_window_placement(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, SW_SHOWMAXIMIZED, WINDOWPLACEMENT};
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if unsafe { GetWindowPlacement(self.hwnd, &mut wp) }.is_ok() {
            let r = wp.rcNormalPosition;
            self.settings.window = Some(super::settings::Placement {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
                maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32,
            });
        }
    }
}
