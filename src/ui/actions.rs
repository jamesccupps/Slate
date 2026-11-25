//! Input handling, commands, file operations and background jobs.
//!
//! App methods never open modal UI (menus, dialogs) because they run while the App is borrowed; they queue a
//! `Deferred` instead, which `run` performs after the borrow ends.

use crate::edit::{Composing, Sink, convert_eol, lines_done, nothing_to_clean};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyMenu, DestroyWindow, GetCaretBlinkTime, GetSystemMetrics, KillTimer, SM_CXDOUBLECLK, SM_CXDRAG, SM_CYDRAG, SPI_GETCARETTIMEOUT,
    SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetTimer, SystemParametersInfoW,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, TrackPopupMenuEx,
};
use windows::core::{HSTRING, PCWSTR};

use crate::core::document::{Document, EditKind, Sel};
use crate::core::io::{self as fileio, Loading, SaveError};
use crate::core::job::{Job, Notify};
use crate::core::csv;
use crate::core::json::{self, Mode as JsonMode};
use crate::core::lines::{self, CaseOp, LineOp};
use crate::core::xml;
use crate::core::search::{self, Matcher};
use crate::core::source::{IndexBuilder, Source};
use crate::core::text::{Encoding, Eol, is_continuation};

use super::app::*;
use super::commands::*;
use super::editor::{self, Ctx, DragMode, View};
use super::findbar::{FindBar, Mode as BarMode, Part};
use super::highlight::Lang;
use super::session::{self, RestoreJob, Restoring, SessionTab};
use super::update::{self, Release, Version};
use super::settings::{ThemeMode, data_dir};
use super::win;

pub const WM_APP_JOB: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
/// The look at the open files on disk (`check_disk`) is done: unlike the other jobs, nothing repaints for it unless it
/// found something (every 2 s, even in a window in the background).
pub const WM_APP_DISK: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
/// The graphics card's Direct3D device is ready (`gfx::make_device_early`): a window drawn with WARP while its driver
/// loaded goes on to it at the next paint.
pub const WM_APP_GPU: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;
pub const TIMER_CARET: usize = 1;
pub const TIMER_JOBS: usize = 2;
pub const TIMER_DISK: usize = 3;
pub const TIMER_SCROLL: usize = 4;
pub const TIMER_SEARCH: usize = 5;
pub const TIMER_SESSION: usize = 6;
/// A while after starting: look for a new version (at most once a day).
pub const TIMER_UPDATE: usize = 7;
/// A status bar message has had its time: repaint without it (nothing else may repaint meanwhile).
pub const TIMER_FLASH: usize = 8;
/// The typing has paused: count the words of a bigger document again (see `App::doc_counts_now`).
pub const TIMER_COUNT: usize = 9;
/// The mouse has rested on a part with a tooltip: show it.
pub const TIMER_TIP: usize = 10;
/// The typing has paused: the JSON/XML path bar and structure panel are worked out again (`Structure::waiting`).
pub const TIMER_STRUCTURE: usize = 11;
/// The window kept off the screen until its first frame (`win::cloak`) is shown by now in any case.
pub const TIMER_UNCLOAK: usize = 12;
/// The window has shown: spell check's checker is made (`App::speller`).
pub const TIMER_SPELL: usize = 13;
/// Drawing couldn't start (the graphics device was lost, during a resize say): paint again from a fresh target.
pub const TIMER_REPAINT: usize = 14;
/// The clicks in the text that close the find bar are over (`App::close_find_later`).
pub const TIMER_FIND_CLOSE: usize = 15;
/// F7's next part (`App::f7`).
pub const TIMER_F7: usize = 16;

const BIG_CLIPBOARD: u64 = 64 << 20;
/// What the status bar says when the clipboard couldn't be written.
const CLIPBOARD_BUSY: &str = "Couldn't use the clipboard (another program may be holding it).";
/// What the status bar says while a check for updates the user asked for runs.
const CHECKING: &str = "Checking for updates…";
/// How the status bar's messages about a file whose lines are still being read start.
const STILL_READING: &str = "Still reading the file's lines";

thread_local! {
    /// Test mode: where the mouse pointer is (client DIPs), for scrolling while dragging.
    pub static TEST_POINTER: std::cell::Cell<Option<(f32, f32)>> = const { std::cell::Cell::new(None) };
}

/// Runs a background transform; if part of the text couldn't be read meanwhile (the file shrank or went away),
/// the result is thrown away instead of producing text with zeros in it. The same if another program wrote into
/// the file the text is read from: the result would mix its version with the user's.
fn guarded(snap: &crate::core::buffer::Snapshot, f: impl FnOnce() -> TaskResult) -> TaskResult {
    let before = snap.read_errors();
    let r = f();
    if matches!(r, TaskResult::Cancelled) {
        return r;
    }
    if snap.read_errors() != before {
        return TaskResult::Failed(
            "Part of the file couldn't be read (was it changed or removed?), so nothing was changed.".into(),
        );
    }
    if snap.changed_in_place() {
        return TaskResult::Failed(
            "Another program changed this file while it was open, so nothing was changed. Reload it first.".into(),
        );
    }
    r
}

/// A file's name, for messages.
fn name_of(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Whether tab `t` has the file `p` (whose canonical form is `canon`, if known) open: by the canonical path its
/// own was found to have (on another thread, when it was read or saved: never asked here, as that's slow, or stuck,
/// on a network drive that went away), else by name.
fn has_file(t: &Tab, p: &Path, canon: Option<&Path>) -> bool {
    let Some(path) = &t.doc.path else { return false };
    match (t.canon.as_ref().filter(|c| &c.0 == path).and_then(|c| c.1.as_deref()), canon) {
        (Some(a), Some(b)) => a == b,
        _ => path.as_os_str().eq_ignore_ascii_case(p.as_os_str()),
    }
}

/// In test mode, `SLATE_TEST_SLOW_OPEN=<ms>` makes reading a file take that long more (a slow network drive).
fn test_slowness() -> Option<Duration> {
    if !win::SCRIPTED.with(|s| s.borrow().is_some()) {
        return None;
    }
    std::env::var("SLATE_TEST_SLOW_OPEN").ok()?.parse().ok().map(Duration::from_millis)
}

/// How long opening a file waits for it to be read before its tab shows "Opening…": most are read by then, and so
/// come up whole at once.
const OPEN_WAIT: Duration = Duration::from_millis(150);

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
        let line_map = tab.filter.as_ref().map(|f| &*f.map);
        let cx = Ctx { doc: &tab.doc, g: &self.g, style: &self.style, theme: &self.theme, lang: tab.lang, geom, line_map };
        f(&mut tab.view, &cx)
    }

    pub fn restart_caret(&mut self) {
        self.caret_on = true;
        caret_moved();
        unsafe {
            let blink = GetCaretBlinkTime();
            // Blinking turned off in Windows' settings (INFINITE), or the keyboard is elsewhere: a steady caret and no
            // timer repainting the window.
            if blink == 0 || blink == u32::MAX || win::focus() != self.hwnd {
                let _ = KillTimer(self.hwnd, TIMER_CARET);
            } else {
                SetTimer(self.hwnd, TIMER_CARET, blink.clamp(200, 2000), None);
            }
        }
    }

    /// A key or the IME in the text, also one that moves nothing (Ctrl+C, Ctrl+Up): the caret shows, and blinks again
    /// if it had stopped.
    pub fn wake_caret(&mut self) {
        let shown = self.caret_on;
        self.restart_caret();
        if !shown {
            self.invalidate();
        }
    }

    /// The mouse capture was lost before the button came up (Alt+Tab, a menu or a dialog took it): end every drag.
    pub fn cancel_drags(&mut self) {
        self.kill_timer(TIMER_SCROLL);
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.view.drag = None;
        }
        self.tab_drag = None;
        self.down = Hit::None;
        self.middle_down = Hit::None;
        self.struct_drag = None;
        if self.split_drag.take().is_some() {
            self.settings.save();
        }
        self.invalidate();
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

    // ---- indentation ----

    /// How tab `i` is indented: what the user picked for it; tabs for TSV files and makefiles (where a tab means
    /// something); what its text uses; else the settings' default.
    pub fn indent_of(&self, i: usize) -> Indent {
        let t = &self.tabs[i];
        match t.indent {
            Some(ind) if t.indent_picked => ind,
            _ if needs_tabs(t) => Indent::Tabs,
            Some(ind) => ind,
            None if self.settings.use_spaces => Indent::Spaces(self.settings.tab_size),
            None => Indent::Tabs,
        }
    }

    pub fn indent_now(&self) -> Indent {
        self.indent_of(self.active)
    }

    /// What EditorConfig says for `path` (tab `i`'s file, or where it's being saved), if it's known yet: a file read
    /// brings it (`Opened::editorconfig`); else it's looked up on another thread (never on this one: a folder on a
    /// share that dropped can take a minute to answer), and the indentation is worked out again once it's there.
    fn editorconfig_for(&mut self, i: usize, path: &Path) -> Option<crate::editorconfig::Props> {
        let notify = self.notify.clone();
        let t = &mut self.tabs[i];
        if let Some((p, props)) = &t.editorconfig {
            if p == path {
                return Some(*props);
            }
        }
        if !t.editorconfig_job.as_ref().is_some_and(|(p, _)| p == path) {
            let p = path.to_path_buf();
            t.editorconfig_job = Some((p.clone(), Job::spawn(0, notify, move |_| crate::editorconfig::for_file(&p))));
            self.timer(TIMER_JOBS, 100);
        }
        None
    }

    /// Looks at how tab `i`'s text is indented (after opening or loading it), unless the user picked it.
    fn detect_indent(&mut self, i: usize) {
        let width = self.settings.tab_size;
        // EditorConfig first, the project's own rule (and for a new file, its line endings and encoding). (Not looked
        // up for a tab whose file is about to be read, empty until then: reading it brings it.)
        let t = &self.tabs[i];
        let ec = match t.doc.path.clone() {
            Some(p) if !t.doc.is_empty() || t.doc.is_dirty() || t.editorconfig.is_some() => {
                self.editorconfig_for(i, &p).unwrap_or_default()
            }
            _ => Default::default(),
        };
        let (ec, eol, charset) = editor::editorconfig_of(&ec, width);
        let t = &mut self.tabs[i];
        if t.doc.is_empty() && !t.doc.is_dirty() {
            if let Some(e) = eol {
                t.doc.eol = e;
            }
            if let Some(c) = charset {
                t.doc.encoding = c;
                t.doc.bom = matches!(c, Encoding::Utf8Bom | Encoding::Utf16Le | Encoding::Utf16Be);
            }
        }
        if !t.indent_picked {
            if ec.is_some() {
                t.indent = ec;
                return;
            }
            let head = t.doc.read(0, t.doc.len().min(256 << 10));
            t.indent = editor::detect_indent(&head);
        }
    }

    // ---- tabs ----

    pub fn add_tab(&mut self, doc: Document) -> usize {
        let id = self.new_tab_id();
        let mut tab = Tab::new(id, doc);
        let head = tab.doc.read(0, 4096);
        let name = tab.doc.path.as_ref().map(|p| p.to_string_lossy().into_owned());
        tab.lang = Lang::detect(name.as_deref(), &head);
        if tab.doc.path.is_none() {
            let used: Vec<u32> = self.tabs.iter().filter(|t| t.doc.path.is_none()).map(|t| t.untitled).collect();
            tab.untitled = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        }
        self.tabs.push(tab);
        let i = self.tabs.len() - 1;
        self.detect_indent(i);
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
        self.follow_find_bar();
        self.reveal_active_tab();
        self.update_title();
        if self.find.open {
            self.schedule_count();
        }
        self.restart_caret();
        self.layout();
        self.apply_goto(i);
        self.invalidate();
    }

    /// A find bar for each tab (`Settings::find_per_tab`): the bar's state goes with the tab shown (a new tab's is
    /// closed; Go to's bar isn't kept); the search itself is shared.
    fn follow_find_bar(&mut self) {
        let id = self.tab().id;
        let old = std::mem::replace(&mut self.find_owner, id);
        if old == id || !self.settings.find_per_tab {
            return;
        }
        let state = (self.find.open && self.find.mode != BarMode::GoTo).then_some(self.find.mode);
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == old) {
            t.find_bar = state;
        }
        match self.tab().find_bar {
            Some(mode) => {
                // (the keyboard in a box this tab's bar doesn't show, Go to's or Replace's, goes to the find box)
                let f = win::focus();
                let hidden = self.find.open
                    && (f == self.find.goto_edit || (f == self.find.replace_edit && mode != BarMode::Replace));
                self.find.open = true;
                self.find.mode = mode;
                if hidden {
                    FindBar::focus(self.find.find_edit);
                }
            }
            None if self.find.open => {
                let in_bar = self.in_find_bar();
                self.find.open = false;
                crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
                self.find.history_at = None;
                if in_bar {
                    win::set_focus(self.hwnd);
                }
            }
            None => {}
        }
    }

    /// Whether the keyboard is in one of the find bar's boxes.
    fn in_find_bar(&self) -> bool {
        let f = win::focus();
        self.find.open && [self.find.find_edit, self.find.replace_edit, self.find.goto_edit].contains(&f)
    }

    /// Goes where tab `i` was asked to go (`Tab::goto`), once it's the tab shown and its document is ready.
    pub fn apply_goto(&mut self, i: usize) {
        if i != self.active || i >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[i];
        let Some(goto) = tab.goto else { return };
        // (still being read, or a tab from the session that isn't back yet)
        if tab.load_job.is_some() || tab.restore.is_some() {
            return;
        }
        match goto {
            Goto::Line(line, col) => {
                // (The lines of a big file are still being counted: once they are.)
                let Some(pos) = line_col_pos(&tab.doc, line, col) else { return };
                tab.goto = None;
                self.go_to(pos, true);
            }
            Goto::Place { caret, top } => {
                tab.goto = None;
                // (the file may be shorter now)
                let len = tab.doc.len();
                let v = &mut tab.view;
                v.sel = Sel::at(editor::char_start(&tab.doc, caret.min(len)));
                v.top = editor::char_start(&tab.doc, top.min(len));
                v.upstream = false;
                v.want_x = None;
                self.restart_caret();
                self.invalidate();
            }
        }
    }

    pub fn update_title(&self) {
        let t = match self.tabs.get(self.active) {
            Some(tab) => {
                let dirty = if tab.doc.is_dirty() { "*" } else { "" };
                // The file's folder is shown here (the tab has just the name): "*notes.txt - C:\work - Slate".
                match tab.doc.path.as_deref().and_then(|p| p.parent()).filter(|_| tab.title_override.is_none()) {
                    Some(dir) => format!("{dirty}{} - {} - Slate", tab.title(), dir.display()),
                    None => format!("{dirty}{} - Slate", tab.title()),
                }
            }
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

    /// Closes tab `i` without asking anything. One with a file (read from or saved to disk) can be opened again
    /// with Reopen closed tab.
    pub fn remove_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        let id = t.id;
        if !self.waits.is_empty() {
            self.tab_gone(t.id);
        }
        // (a tab Replace in all tabs was replacing in: done, as closed)
        if self.replacing.is_some() {
            self.replaced_check();
        }
        // (big piped-in text, opened as the file it was put in: that goes with its tab, and can't be reopened)
        if let Some(path) = t.doc.path.clone().filter(|p| super::handed_text(p)) {
            drop(t);
            let _ = std::fs::remove_file(&path);
        } else if let (Some(path), Some(_), None) = (&t.doc.path, t.doc.disk, &t.title_override) {
            self.settings.remember_place(path, t.view.sel.caret, t.view.top);
            self.closed_tabs.retain(|c| &c.path != path);
            self.closed_tabs.push(ClosedTab { path: path.clone(), caret: t.view.sel.caret, top: t.view.top });
            if self.closed_tabs.len() > 20 {
                self.closed_tabs.remove(0);
            }
        }
        if self.tabs.is_empty() {
            self.new_untitled();
            // (the user closed the last tab, and Slate goes with it: Settings → Close Slate with the last tab)
            if self.quit_with_tab.take() == Some(id) && self.settings.close_with_last_tab {
                self.pending.push(Deferred::Cmd(Cmd::Exit));
            }
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        let a = self.active;
        self.activate(a);
        self.session_dirty = true;
    }

    // ---- files ----

    /// Opens files named on a command line (Slate's own, or another Slate's that handed them over). A name that
    /// isn't there but ends in `:line` or `:line:column` (`notes.txt:120`) opens that file at that line.
    pub fn open_command_line(&mut self, paths: &[PathBuf]) {
        // (A message from before stays, also when these change the tab shown: what restoring the session said at the
        // start, or one about another tab while another Slate hands these over.)
        let said = self.flash.take();
        let mut started = Vec::new();
        for p in paths {
            let p = std::path::absolute(p).unwrap_or_else(|_| p.clone());
            // A name that isn't there yet becomes a new file, but not one with a colon in it: saving that would write
            // an alternate data stream of another file.
            let creatable = |f: &Path| !f.file_name().is_some_and(|n| n.to_string_lossy().contains(':'));
            let Some((file, line, col)) = line_suffix(&p) else {
                started.extend(self.open_path(&p, creatable(&p), None, None));
                continue;
            };
            // `notes.txt:120`: that file, at that line (unless a file has that very name, which the reading finds
            // out: the tab then gets the file that was read). Open already: there.
            let goto = Some(Goto::Line(line, col));
            if let Some(i) = self.tabs.iter().position(|t| has_file(t, &file, None)) {
                self.tabs[i].goto = goto;
                self.activate(i);
                continue;
            }
            let create = creatable(&file);
            if let Some(id) = self.open_path(&p, false, Some((file, create)), None) {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    t.goto = goto;
                }
                started.push(id);
            }
        }
        self.settle(&started);
        let a = self.active;
        self.activate(a);
        super::keep_message(self, said);
    }

    /// Opens what a command line asks for: its text from standard input in a new tab, its files (as
    /// `open_command_line`), read only if asked, and with `--wait` notes the tabs, so the Slate waiting hears when
    /// they're all closed.
    pub fn open_request(&mut self, paths: &[PathBuf], o: &Opts) {
        if *o == Opts::default() {
            self.open_command_line(paths);
            return;
        }
        let before: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        if let Some(text) = &o.text {
            let blank = self.tabs.len() == 1 && self.tabs[0].is_blank();
            let mut doc = Document::from_text(text);
            doc.eol = crate::core::text::detect_eol(&text[..text.len().min(64 << 10)]);
            doc.mark_dirty();
            let i = self.add_tab(doc);
            if blank && i == 1 {
                self.tabs.remove(0);
                self.active = 0;
                // ("Untitled", the blank one's name, is free now)
                self.tabs[0].untitled = 1;
            }
        }
        if !paths.is_empty() {
            self.open_command_line(paths);
        }
        // (the tabs of those files: new ones, and ones that had them open already)
        let wanted: Vec<PathBuf> = paths
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .map(|p| line_suffix(&p).map_or(p.clone(), |s| s.0))
            .collect();
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| !before.contains(&t.id) || wanted.iter().any(|p| has_file(t, p, None)))
            .map(|t| t.id)
            .collect();
        if o.read_only {
            for t in self.tabs.iter_mut().filter(|t| ids.contains(&t.id)) {
                t.read_only = true;
            }
            self.layout();
        }
        if let Some(name) = &o.wait {
            if ids.is_empty() {
                signal_wait(name);
            } else {
                self.waits.push((name.clone(), ids));
            }
        }
        self.session_dirty = true;
        self.invalidate();
    }

    /// Tab `id` closed: a `--wait` all of whose tabs are closed now is told.
    fn tab_gone(&mut self, id: u64) {
        for (_, ids) in self.waits.iter_mut() {
            ids.retain(|&t| t != id);
        }
        self.waits.retain(|(name, ids)| {
            if ids.is_empty() {
                signal_wait(name);
            }
            !ids.is_empty()
        });
    }

    /// Every `--wait` is told (the window is closing).
    pub fn end_waits(&mut self) {
        for (name, _) in std::mem::take(&mut self.waits) {
            signal_wait(name.as_str());
        }
    }

    /// Opens files in new tabs. Each is read on another thread (a network drive can take long to answer); its tab
    /// shows "Opening…" until it is, unless that's done within `OPEN_WAIT`.
    pub fn open_paths(&mut self, paths: &[PathBuf]) {
        let started: Vec<u64> = paths.iter().filter_map(|p| self.open_path(p, false, None, None)).collect();
        self.settle(&started);
        let a = self.active;
        self.activate(a);
    }

    /// Starts opening the file of a tab from the session (`st`) like `open_paths` (`settle` waits for it): once
    /// it's read, the tab is where it was; if it can't be read just now, the tab waits for it (and stays in the
    /// session). Returns the new tab's id.
    pub fn open_from_session(&mut self, st: &SessionTab) -> Option<u64> {
        let p = st.path.as_ref()?;
        let place = SessionTab { backup: None, pieces: None, ..st.clone() };
        self.open_path(p, false, None, Some(place))
    }

    /// Starts opening `p` in a new tab (`create`: one that isn't there can be a new file; `or`: if `p` isn't there,
    /// that one instead, and whether it can be a new file; `place`: see `Tab::place`). Returns its id; None if a tab
    /// has that file already (shown instead).
    fn open_path(
        &mut self,
        p: &Path,
        create: bool,
        or: Option<(PathBuf, bool)>,
        place: Option<SessionTab>,
    ) -> Option<u64> {
        let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        // (Open under another name: found once it's read, see `finish_read`.)
        if let Some(i) = self.tabs.iter().position(|t| has_file(t, &p, None)) {
            self.activate(i);
            return None;
        }
        // Opening into a single blank tab replaces it, like Notepad.
        let replace_blank = self.tabs.len() == 1 && self.tabs[0].is_blank();
        let from = self.tabs.get(self.active).map(|t| t.id).filter(|_| !replace_blank);
        // (where it was left, unless it's a tab from the session, which has its own place)
        let left = if place.is_none() { self.settings.place_of(&p) } else { None };
        let mut doc = Document::new();
        doc.path = Some(p);
        let mut i = self.add_tab(doc);
        self.tabs[i].opened_from = from;
        self.tabs[i].goto = left.map(|(caret, top)| Goto::Place { caret, top });
        if replace_blank && i == 1 {
            self.tabs.remove(0);
            self.active = 0;
            i = 0;
        }
        self.tabs[i].place = place;
        self.start_load(i, None, create, or);
        Some(self.tabs[i].id)
    }

    /// Starts reading tab `i`'s file on another thread: opening it (`reload` None; `create` and `or`: see
    /// `open_path`), or reloading it.
    fn start_load(&mut self, i: usize, force: Option<Encoding>, create: bool, or: Option<(PathBuf, bool)>) {
        let Some(path) = self.tabs[i].doc.path.clone() else { return };
        let notify = self.notify.clone();
        let slow = test_slowness();
        let job = match self.tabs[i].reload {
            None => Job::spawn(fileio::OPEN_STEPS, notify.clone(), move |ctx| {
                if let Some(d) = slow {
                    std::thread::sleep(d);
                }
                match or {
                    Some((file, create)) => fileio::open_either(&path, &file, notify, create, ctx),
                    None => fileio::open(&path, notify, create, ctx),
                }
            }),
            Some(_) => {
                // (the file as read before: if it only grew, its newline index is used again)
                let sources = self.tabs[i].doc.buffer().sources();
                let prev = sources.iter().find(|s| s.file_path() == Some(path.as_path())).cloned();
                Job::spawn(fileio::OPEN_STEPS, notify.clone(), move |ctx| {
                    if let Some(d) = slow {
                        std::thread::sleep(d);
                    }
                    fileio::reload(&path, notify, prev, force, ctx)
                })
            }
        };
        self.tabs[i].load_job = Some(Load::Read(job));
        self.timer(TIMER_JOBS, 100);
    }

    /// Waits a moment (`OPEN_WAIT`) for the files tabs `ids` started reading, so one that's quick to read comes up at
    /// once; the others go on in the background.
    pub fn settle(&mut self, ids: &[u64]) {
        let until = Instant::now() + OPEN_WAIT;
        let reading = |a: &App| {
            a.tabs.iter().any(|t| ids.contains(&t.id) && matches!(&t.load_job, Some(Load::Read(j)) if !j.is_finished()))
        };
        while reading(self) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        // (a tab can close here: by id)
        for id in ids {
            if let Some(i) = self.tabs.iter().position(|t| t.id == *id) {
                self.poll_load(i);
            }
        }
    }

    /// Takes a finished read of tab `i`'s file; returns whether one is still going. The tab may be gone after.
    fn poll_load(&mut self, i: usize) -> bool {
        match self.tabs[i].load_job.as_mut() {
            Some(Load::Read(job)) => {
                let Some(opened) = job.take() else { return true };
                self.tabs[i].load_job = None;
                self.finish_read(i, opened)
            }
            Some(Load::Convert(job)) => {
                let Some(r) = job.take() else { return true };
                self.tabs[i].load_job = None;
                match r {
                    Ok(doc) => self.loaded(i, doc, None),
                    Err(e) => self.load_failed(i, e, false),
                }
                false
            }
            None => false,
        }
    }

    /// Tab `i`'s file was read (or not); returns whether it still is (a big file being converted).
    fn finish_read(&mut self, i: usize, opened: fileio::Opened) -> bool {
        // (the file it read: `notes.txt` for `notes.txt:120`)
        if self.tabs[i].reload.is_none() && !opened.path.as_os_str().is_empty() {
            self.tabs[i].doc.path = Some(opened.path.clone());
        }
        if !opened.path.as_os_str().is_empty() {
            self.tabs[i].editorconfig = Some((opened.path.clone(), opened.editorconfig));
        }
        let loading = match opened.loading {
            Ok(l) => l,
            Err(e) => {
                self.load_failed(i, e, opened.creatable);
                return false;
            }
        };
        if self.tabs[i].reload.is_none() {
            let id = self.tabs[i].id;
            let path = self.tabs[i].doc.path.clone().unwrap_or_default();
            // Open in another tab already, under another name (the same canonical path): that one, then.
            let canon = opened.canon.as_deref();
            if let Some(other) = self.tabs.iter().find(|t| t.id != id && has_file(t, &path, canon)).map(|t| t.id) {
                let goto = self.tabs[i].goto.take();
                // (a `--wait` for this tab waits for that one)
                for (_, ids) in self.waits.iter_mut().filter(|(_, ids)| ids.contains(&id)) {
                    ids.retain(|&t| t != id);
                    ids.push(other);
                }
                self.remove_tab(i);
                if let Some(k) = self.tabs.iter().position(|t| t.id == other) {
                    self.tabs[k].goto = goto.or(self.tabs[k].goto);
                    self.activate(k);
                }
                return false;
            }
            self.tabs[i].canon = Some((path.clone(), opened.canon));
            if opened.read_only {
                self.tabs[i].read_only = true;
            }
            // Binary files open too (as text), but saving one from here could damage it.
            self.tabs[i].binary = opened.binary;
            if opened.binary {
                self.tabs[i].notice = Some(Notice {
                    kind: NoticeKind::Warn,
                    text: "This looks like a binary file, not text. Saving it from Slate could damage it.".into(),
                    actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                });
                self.layout();
            }
            self.settings.add_recent(&path);
            super::jumplist::update(self.settings.recent.clone());
            // (once for files opened together)
            if !self.tabs.iter().any(|t| matches!(t.load_job, Some(Load::Read(_)))) {
                self.settings.save();
            }
        }
        match loading {
            Loading::Ready(doc) => self.loaded(i, doc, None),
            Loading::Indexing(doc, job) => self.loaded(i, doc, Some(job)),
            Loading::Converting(job) => {
                self.tabs[i].load_job = Some(Load::Convert(job));
                return true;
            }
        }
        false
    }

    /// Tab `i`'s file is read: its document from now on.
    fn loaded(&mut self, i: usize, doc: Document, index_job: Option<Job<bool>>) {
        match self.tabs[i].reload.take() {
            Some(follow_end) => self.replace_doc(i, doc, index_job, follow_end),
            None => {
                let tab = &mut self.tabs[i];
                tab.doc = doc;
                tab.index_job = index_job;
                tab.backup_version = u64::MAX;
                tab.view.forget_text();
                if !tab.lang_picked {
                    let head = tab.doc.read(0, 4096);
                    let name = tab.doc.path.as_ref().map(|p| p.to_string_lossy());
                    tab.lang = Lang::detect(name.as_deref(), &head);
                }
                self.detect_indent(i);
            }
        }
        // (a tab from the session: back where it was; one that waited for its file is back)
        if let Some(st) = self.tabs[i].place.take() {
            super::place_tab(&mut self.tabs[i], &st);
        }
        self.tabs[i].restore = None;
        if self.tabs[i].index_job.is_some() {
            self.timer(TIMER_JOBS, 100);
        } else if i == self.active {
            self.drop_flash(STILL_READING);
        }
        if i == self.active {
            self.drop_flash("Still opening the file");
        }
        self.session_dirty = true;
        self.update_title();
        self.invalidate();
    }

    /// Tab `i`'s file couldn't be read (`e`). `creatable`: it isn't there, but can be a new file.
    fn load_failed(&mut self, i: usize, e: io::Error, creatable: bool) {
        let name = self.tabs[i].title();
        let place = self.tabs[i].place.take();
        if self.tabs[i].reload.take().is_some() {
            self.tabs[i].notice = Some(Notice {
                kind: NoticeKind::Error,
                text: format!("Couldn't reload the file: {}", fileio::friendly_io(&e)),
                actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
            });
            self.layout();
        } else if creatable {
            // A file that isn't there yet in a folder that is (`Slate todo.txt`): an empty tab that becomes that
            // file when it's saved (Notepad offers to create it), with EditorConfig's line endings, encoding and
            // indentation for it (the open looked them up: `finish_read`).
            self.flash(format!("{name} is a new file: saving creates it."), false);
            self.detect_indent(i);
        } else if (place.is_some() || self.tabs[i].restore.is_some())
            && self.tabs[i].doc.path.as_deref().is_some_and(|p| fileio::transient(p, &e))
        {
            // A tab from the session whose file can't be read just now (another program has it, a network drive or
            // a USB stick isn't there): it waits for it, and stays. (Waiting already: the tries count on.)
            if let Some(st) = place.filter(|_| self.tabs[i].restore.is_none()) {
                self.tabs[i].restore = Some(Restoring::new(st, None));
            }
            self.wait_for_file(i, Some(fileio::friendly_io(&e)));
        } else {
            // (back to the tab it was opened from, not whichever is beside it)
            let back = self.tabs[i].opened_from.filter(|_| i == self.active);
            // (the whole path: it says where Slate looked)
            let full = self.tabs[i].doc.path.as_deref().map_or_else(|| name.clone(), |p| p.display().to_string());
            self.tabs[i].restore = None;
            self.remove_tab(i);
            if let Some(k) = back.and_then(|id| self.tabs.iter().position(|t| t.id == id)) {
                self.activate(k);
            }
            self.flash(format!("Couldn't open {full}: {}", fileio::friendly_io(&e)), true);
        }
        self.invalidate();
    }

    /// Re-reads tab `i` from disk (keeping the view where it was), on another thread like opening it.
    pub fn reload(&mut self, i: usize, force: Option<Encoding>) {
        // A tab from the session that isn't back yet: that's what to try again.
        if self.tabs[i].restore.is_some() {
            self.start_restore(i);
            return;
        }
        let tab = &mut self.tabs[i];
        if tab.doc.path.is_none() {
            return;
        }
        if tab.save.is_some() {
            // (what's read now could be from before the save, which then lands on another document)
            self.flash("Still saving — reload when it's done.", true);
            return;
        }
        // (Still being opened: opened again.)
        if tab.load_job.is_none() || tab.reload.is_some() {
            tab.reload = Some(tab.view.sel.caret >= tab.doc.len() && !tab.doc.is_empty());
        }
        let id = tab.id;
        self.start_load(i, force, false, None);
        self.settle(&[id]);
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
        self.filter_again(i);
    }

    // ---- tabs from the session that aren't back yet ----

    /// A tab for a session entry that isn't back yet (session.rs, `Restoring`): a big document put back from its
    /// pieces on another thread (`big`), or a file that doesn't answer yet (tried again now and then). Returns its
    /// index.
    pub fn add_restoring(&mut self, st: SessionTab, big: Option<session::BigList>) -> usize {
        let mut doc = Document::new();
        doc.path = st.path.clone();
        doc.encoding = st.encoding;
        doc.bom = st.bom;
        doc.eol = st.eol;
        doc.disk = session::disk_from(&st);
        let i = self.add_tab(doc);
        let tab = &mut self.tabs[i];
        if st.untitled > 0 && st.path.is_none() {
            tab.untitled = st.untitled;
            self.untitled_counter = self.untitled_counter.max(st.untitled);
        }
        if st.lang_picked {
            tab.lang = st.lang;
            tab.lang_picked = true;
        }
        let waiting = big.is_none();
        let tab = &mut self.tabs[i];
        tab.restore = Some(Restoring::new(st, big));
        if waiting {
            self.wait_for_file(i, None);
        } else {
            self.start_restore(i);
        }
        i
    }

    /// Starts putting tab `i` back (a big document), or looking whether its file answers yet.
    fn start_restore(&mut self, i: usize) {
        let notify = self.notify.clone();
        // (its file being read already: that's the try)
        if self.tabs[i].load_job.is_some() {
            return;
        }
        let Some(r) = self.tabs[i].restore.as_mut() else { return };
        if r.running() {
            return;
        }
        r.retry_at = None;
        match (r.big.clone(), r.st.pieces.clone(), r.st.path.clone()) {
            // (its list wasn't read yet if it couldn't be at the start: read it there)
            (list, Some(name), _) => {
                let total = list.as_ref().map_or(0, |l| l.total());
                let job = Job::spawn(total, notify, move |ctx| session::restore_big(&name, list.as_deref(), ctx));
                r.job = Some(RestoreJob::Big(job));
            }
            (_, None, Some(path)) => {
                r.job = Some(RestoreJob::Probe(Job::spawn(0, notify, move |_| session::probe(&path))));
            }
            _ => return,
        }
        self.timer(TIMER_JOBS, 100);
    }

    /// Tab `i`'s file doesn't answer (`why`, if known): look again later, and say so.
    fn wait_for_file(&mut self, i: usize, why: Option<String>) {
        let tab = &mut self.tabs[i];
        let Some(r) = tab.restore.as_mut() else { return };
        r.wait();
        let name = r.st.path.as_deref().map(name_of).unwrap_or_else(|| "Its file".into());
        // ("Another program is using the file." → " (another program is using the file)")
        let why = why
            .map(|w| {
                let w = w.trim_end_matches('.');
                let mut c = w.chars();
                let first = c.next().map(|f| f.to_lowercase().collect::<String>()).unwrap_or_default();
                format!(" ({first}{})", c.as_str())
            })
            .unwrap_or_default();
        tab.notice = Some(if r.is_big() {
            Notice {
                kind: NoticeKind::Warn,
                text: format!(
                    "Unsaved changes from last time wait for {name}, which can't be read just now{why}. Slate puts \
                     them back as soon as it can."
                ),
                actions: vec![
                    ("Try now".into(), NoticeAction::Retry),
                    ("Get the added text now".into(), NoticeAction::Recover),
                ],
            }
        } else {
            Notice {
                kind: NoticeKind::Info,
                text: if why.is_empty() {
                    format!("{name} doesn't answer (a network drive?). Slate opens it as soon as it does.")
                } else {
                    format!("{name} can't be read just now{why}. Slate opens it as soon as it can.")
                },
                actions: vec![("Try now".into(), NoticeAction::Retry)],
            }
        });
        self.layout();
    }

    /// Looks again for files that didn't answer, when it's time (from `check_disk`).
    fn retry_restores(&mut self) {
        let now = Instant::now();
        for i in 0..self.tabs.len() {
            if self.tabs[i].restore.as_ref().is_some_and(|r| !r.running() && r.retry_at.is_some_and(|at| now >= at)) {
                self.start_restore(i);
            }
        }
    }

    /// Takes a finished restore or look of tab `i`; returns whether one is still running.
    fn poll_restore(&mut self, i: usize) -> bool {
        let Some(r) = self.tabs[i].restore.as_mut() else { return false };
        match r.job.as_mut() {
            Some(RestoreJob::Big(job)) => match job.take() {
                None => true,
                Some(back) => {
                    r.job = None;
                    self.finish_restore(i, back);
                    false
                }
            },
            Some(RestoreJob::Probe(job)) => match job.take() {
                None => true,
                Some(answer) => {
                    r.job = None;
                    self.finish_probe(i, answer);
                    false
                }
            },
            None => false,
        }
    }

    /// A big document's tab, put back (or not) from its pieces.
    fn finish_restore(&mut self, i: usize, back: session::Restored) {
        let Some(r) = self.tabs[i].restore.as_ref() else { return };
        let st = r.st.clone();
        let name = st.pieces.clone().unwrap_or_default();
        let file = st.path.as_deref().map(name_of).unwrap_or_else(|| self.tabs[i].title());
        match back {
            session::Restored::Ready { mut doc, kept, canon } => {
                doc.path = st.path.clone();
                doc.encoding = st.encoding;
                doc.bom = st.bom;
                doc.eol = st.eol;
                doc.disk = session::disk_from(&st);
                doc.mark_dirty();
                let tab = &mut self.tabs[i];
                tab.big = Some(kept);
                tab.canon = st.path.clone().map(|p| (p, canon));
                tab.restore = None;
                tab.notice = None;
                tab.doc = doc;
                tab.backup_version = u64::MAX;
                tab.view.forget_text();
                let head = tab.doc.read(0, 4096);
                tab.lang = Lang::detect(st.path.as_ref().map(|p| p.to_string_lossy()).as_deref(), &head);
                self.detect_indent(i);
                super::place_tab(&mut self.tabs[i], &st);
            }
            session::Restored::Recovered { doc, why, there } => self.recovered(i, &name, &file, doc, &why, there),
            session::Restored::Unreachable(why) => self.wait_for_file(i, Some(why)),
            session::Restored::Damaged(why) => {
                // Kept for a look, never deleted; the file as it is in this tab.
                let kept = session::set_aside_big(&name);
                self.tabs[i].restore = None;
                self.reopen_as_it_is(i);
                self.tabs[i].notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: format!(
                        "Unsaved changes to {file} from last time couldn't be read back ({why}). Slate kept them in \
                         {}.",
                        kept.display()
                    ),
                    actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
                });
                self.layout();
            }
        }
        self.session_dirty = true;
        self.update_title();
        self.invalidate();
    }

    /// A big document's changes can't be laid over its file any more (`why`): tab `i` becomes the text that was
    /// added to it, on its own (the session's copy goes into `damaged\`), and the file, if it's `there`, opens in a
    /// tab as it is now.
    fn recovered(&mut self, i: usize, name: &str, file: &str, mut doc: Document, why: &str, there: bool) {
        let kept = session::set_aside_big(name);
        let path = self.tabs[i].restore.take().and_then(|r| r.st.path);
        let added = !doc.is_empty();
        doc.mark_dirty();
        let tab = &mut self.tabs[i];
        tab.doc = doc;
        tab.big = None;
        tab.view.forget_text();
        tab.view.sel = Sel::at(0);
        tab.view.top = 0;
        let used: Vec<u32> = self.tabs.iter().filter(|t| t.doc.path.is_none()).map(|t| t.untitled).collect();
        let tab = &mut self.tabs[i];
        tab.untitled = (1..).find(|n| !used.contains(n)).unwrap_or(1);
        tab.notice = Some(Notice {
            kind: NoticeKind::Warn,
            text: if added {
                format!(
                    "Unsaved changes to {file} from last time couldn't be put back: {why}. This is the text that was \
                     added to it, each part with where it was; Slate also kept its copy of the changes in {}.",
                    kept.display()
                )
            } else {
                format!(
                    "Unsaved changes to {file} from last time couldn't be put back: {why}. Slate kept them in {}.",
                    kept.display()
                )
            },
            actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
        });
        self.layout();
        // (and the file as it is now, if it's there)
        if let Some(p) = path.filter(|_| there) {
            let id = self.tabs[i].id;
            self.open_paths(&[p]);
            if let Some(k) = self.tabs.iter().position(|t| t.id == id) {
                self.activate(k);
            }
        }
    }

    /// A waiting tab's file answered: open it there (or say it's gone).
    fn finish_probe(&mut self, i: usize, answer: Option<bool>) {
        match answer {
            Some(true) => {
                // Read it: back where it was once it's read; if it still can't be, it waits again (it stays a tab
                // from the session until then, so the tries count on).
                let Some(r) = self.tabs[i].restore.as_ref() else { return };
                let st = SessionTab { backup: None, pieces: None, ..r.st.clone() };
                self.tabs[i].notice = None;
                self.tabs[i].place = Some(st);
                self.reopen_as_it_is(i);
            }
            Some(false) => {
                let name = self.tabs[i].title();
                self.tabs[i].restore = None;
                self.remove_tab(i);
                self.flash(format!("{name} isn't there any more."), true);
            }
            None => self.wait_for_file(i, None),
        }
        self.session_dirty = true;
        self.invalidate();
    }

    /// Opens tab `i`'s file into it, as it is on disk (a tab that was waiting for it), or leaves the tab empty.
    fn reopen_as_it_is(&mut self, i: usize) {
        if self.tabs[i].doc.path.is_some() {
            self.tabs[i].reload = None;
            self.start_load(i, None, false, None);
        }
    }

    /// Starts saving tab `i`. Returns false if it can't be saved now.
    pub fn start_save(&mut self, i: usize, path: PathBuf, encoding: Encoding, close_after: bool) -> bool {
        self.start_save_lossy(i, path, encoding, close_after, false)
    }

    /// `start_save`; `lossy_ok`: the user agreed that characters ANSI can't hold become "?", and that parts of a
    /// damaged UTF-16 file stay U+FFFD (`Document::bad_units`); otherwise such a save stops before writing anything
    /// and asks, see `Deferred::AskLossy`.
    pub fn start_save_lossy(
        &mut self,
        i: usize,
        path: PathBuf,
        encoding: Encoding,
        close_after: bool,
        lossy_ok: bool,
    ) -> bool {
        let notify = self.notify.clone();
        // (EditorConfig's insert_final_newline, if it's known for that file yet)
        let final_newline = self.editorconfig_for(i, &path).and_then(|p| p.final_newline) == Some(true);
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
        if !tab.doc.is_ready() || tab.load_job.is_some() || tab.restore.is_some() {
            self.flash("Still opening the file — try saving again in a moment.", true);
            return false;
        }
        // (EditorConfig's insert_final_newline)
        let eol = tab.doc.eol.as_bytes().to_vec();
        if !tab.read_only && tab.filter.is_none() && editor::final_newline(&mut tab.doc, final_newline, &eol) {
            tab.sync_view();
        }
        let snap = tab.doc.snapshot();
        let state = tab.doc.state_id();
        let version = tab.doc.version;
        let p = path.clone();
        let bom = tab.doc.bom;
        let saving = snap.clone();
        let damaged = (tab.doc.bad_units > 0 || tab.doc.suspect) && !lossy_ok;
        // (to its own file: unless another program changed it just now, unseen by the look every 2 s)
        let own = tab.doc.path.as_deref().is_some_and(|q| q.as_os_str().eq_ignore_ascii_case(path.as_os_str()));
        let looked = own.then_some((tab.doc.disk, tab.seen_disk));
        let job = Job::spawn(snap.len(), notify, move |ctx| {
            if damaged {
                return Err(SaveError::Lossy);
            }
            if let Some(now) = looked.and_then(|(old, seen)| fileio::changed_outside(&p, old, seen)) {
                return Err(SaveError::Outside(now));
            }
            fileio::save(&saving, &p, encoding, bom, lossy_ok, false, ctx)
        });
        let doc = tab.doc.id();
        tab.save = Some(SaveTask { job, state, version, snap, doc, path, encoding, close_after, again: false });
        tab.doc.seal();
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
        true
    }

    /// Looks for files changed on disk by other programs. The file system is asked on a background thread (a
    /// network drive that went away can take half a minute to answer, which mustn't freeze the window);
    /// `poll_disk` acts on what it finds.
    pub fn check_disk(&mut self) {
        // (and tabs from the session whose files didn't answer: look again when it's time)
        self.retry_restores();
        // (a look that's done but whose message didn't get through is picked up here)
        if self.disk_job.is_some() && self.poll_disk() {
            return; // the last look hasn't finished (a slow drive): the next one waits for it
        }
        let mut asks = Vec::new();
        for tab in &self.tabs {
            if tab.busy() {
                continue;
            }
            let (Some(path), Some(old)) = (tab.doc.path.clone(), tab.doc.disk) else { continue };
            // Unsaved changes to text read from a file (a big one): did another program write into that file?
            let sources = if tab.doc.is_dirty() { tab.doc.file_sources() } else { Vec::new() };
            asks.push((tab.id, path, old, sources));
        }
        if asks.is_empty() {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        let notify: Notify = Arc::new(move || unsafe {
            use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
            let _ = PostMessageW(HWND(hwnd as *mut _), WM_APP_DISK, WPARAM(0), LPARAM(0));
        });
        self.disk_job = Some(Job::spawn(0, notify, move |_| {
            // (a file that doesn't answer just now says nothing: it isn't taken for one that was deleted)
            asks.into_iter()
                .filter_map(|(id, path, old, sources)| {
                    // (another file put in its place, or moved away: the session can't read it in a later run)
                    let was = sources.iter().filter(|s| s.is_gone()).count();
                    sources.iter().for_each(|s| s.look_at_path());
                    let gone = sources.iter().filter(|s| s.is_gone()).count() > was;
                    let now = fileio::disk_answer(&path)?;
                    Some(DiskCheck { id, old, now, in_place: sources.iter().any(|s| s.changed_in_place()), gone })
                })
                .collect()
        }));
    }

    /// Acts on a finished `check_disk` (repainting only for what it found); returns whether one is still running.
    pub fn poll_disk(&mut self) -> bool {
        let Some(job) = self.disk_job.as_mut() else { return false };
        let Some(found) = job.take() else { return true };
        self.disk_job = None;
        for c in found {
            // (the session copies what's used of a file that isn't at its path any more: written again)
            self.session_dirty |= c.gone;
            let Some(i) = self.tabs.iter().position(|t| t.id == c.id) else { continue };
            let tab = &self.tabs[i];
            // Saved, reloaded or busy since the look started: the next look is what counts.
            if tab.doc.disk != Some(c.old) || tab.busy() {
                continue;
            }
            if (c.now.is_some_and(|n| c.old.same(n)) && !c.in_place) || tab.seen_disk == Some(c.now) {
                continue;
            }
            let dirty = tab.doc.is_dirty();
            if c.now.is_some() && !dirty {
                if tab.index_job.is_some() {
                    // Still reading its lines: reloading now would start that over; it reloads once they're read.
                    self.tabs[i].notice = Some(Notice {
                        kind: NoticeKind::Info,
                        text: "This file changed on disk. Slate reloads it as soon as it has read all its lines."
                            .into(),
                        actions: vec![("Reload now".into(), NoticeAction::Reload)],
                    });
                } else {
                    self.reload(i, None);
                    continue;
                }
            } else {
                let tab = &mut self.tabs[i];
                tab.seen_disk = Some(c.now);
                tab.notice = Some(if c.in_place {
                    // Big files are read from disk as needed: what the user didn't edit is now the other program's.
                    Notice {
                        kind: NoticeKind::Warn,
                        text: "Another program changed this file while it was open. Slate reads big files from disk, \
                               so the parts you didn't edit now show its version, and your changes can't be saved \
                               without mixing the two."
                            .into(),
                        actions: vec![
                            ("Reload (lose my changes)".into(), NoticeAction::Reload),
                            ("Dismiss".into(), NoticeAction::Dismiss),
                        ],
                    }
                } else if c.now.is_none() {
                    Notice {
                        kind: NoticeKind::Warn,
                        text: "This file was deleted or moved. Save it to keep your copy.".into(),
                        actions: vec![
                            ("Save as…".into(), NoticeAction::SaveAs),
                            ("Dismiss".into(), NoticeAction::Dismiss),
                        ],
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
            }
            self.layout();
            self.invalidate();
        }
        false
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
        any |= self.poll_update();
        any |= self.poll_session();
        any |= self.poll_many();
        any |= self.poll_disk();
        any |= self.poll_counts();
        // (A document that just became ready: where it was asked to go.)
        let a = self.active;
        self.apply_goto(a);
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
        if std::mem::take(&mut self.restart_on_exit) {
            self.flash("Slate is updated: the new version starts the next time you open it.", false);
        }
    }

    // ---- updates ----

    /// Asks GitHub for the latest release in the background. `manual`: from the Help menu.
    pub fn check_for_update(&mut self, manual: bool) {
        match self.update {
            UpdateState::Checking { .. } | UpdateState::Downloading { .. } => return,
            UpdateState::Available(_) if manual => {
                self.pending.push(Deferred::UpdatePrompt);
                return;
            }
            _ => {}
        }
        let job = Job::spawn(0, self.notify.clone(), |_| update::latest());
        self.update = UpdateState::Checking { manual, job };
        if manual {
            self.flash(CHECKING, false);
        }
        self.timer(TIMER_JOBS, 100);
    }

    pub fn start_update(&mut self, release: Release) {
        if !update::can_replace() {
            let dir = super::settings::exe_dir().map(|d| d.display().to_string()).unwrap_or_default();
            update::show_page(&release);
            self.flash(format!("Slate can't replace itself in {dir}; the release page is open to download it from."), true);
            return;
        }
        let r = release.clone();
        let job = Job::spawn(release.exe_size, self.notify.clone(), move |ctx| {
            update::download(&r, ctx).and_then(|file| update::install(&file, r.version))
        });
        self.update = UpdateState::Downloading { release, job };
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
    }

    /// Picks up a finished update check or download; returns whether one is still running.
    fn poll_update(&mut self) -> bool {
        match std::mem::replace(&mut self.update, UpdateState::Idle) {
            UpdateState::Checking { manual, mut job } => match job.take() {
                None => {
                    self.update = UpdateState::Checking { manual, job };
                    return true;
                }
                Some(Ok(rel)) => {
                    self.settings.last_update_check = unix_now();
                    self.settings.save();
                    // (One that didn't start on this PC before is only offered when asked for.)
                    if rel.version > Version::current() && (manual || rel.version.to_string() != self.settings.failed_update) {
                        self.update = UpdateState::Available(rel);
                        if manual {
                            // (the prompt and the status bar's "Update to …" say it: not "Checking…" beside it)
                            self.drop_flash(CHECKING);
                            self.pending.push(Deferred::UpdatePrompt);
                        }
                    } else if manual {
                        self.flash(format!("Slate is up to date ({})", Version::current()), false);
                    }
                }
                Some(Err(e)) => {
                    if manual {
                        self.flash(format!("Couldn't check for updates: {e}"), true);
                    }
                }
            },
            UpdateState::Downloading { release, mut job } => match job.take() {
                None => {
                    self.update = UpdateState::Downloading { release, job };
                    return true;
                }
                Some(Ok(())) => {
                    // Close (keeping the tabs in the session) and start the new version.
                    self.flash(format!("Restarting with Slate {}…", release.version), false);
                    self.restart_on_exit = true;
                    self.pending.push(Deferred::Cmd(Cmd::Exit));
                }
                Some(Err(e)) => {
                    if e != "Cancelled" {
                        self.flash(format!("Couldn't update: {e}"), true);
                    }
                    self.update = UpdateState::Available(release);
                }
            },
            other => self.update = other,
        }
        self.invalidate();
        false
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
        // A tab from the session being put back, or looking whether its file answers (it may close).
        let id = self.tabs[i].id;
        running |= self.poll_restore(i);
        if self.tabs.get(i).map(|t| t.id) != Some(id) {
            return running;
        }
        // Reading its file (opening, reloading, converting a big UTF-16 / ANSI file); it may close.
        running |= self.poll_load(i);
        if self.tabs.get(i).map(|t| t.id) != Some(id) {
            return running;
        }
        // EditorConfig looked up: the indentation by it (and a new file's line endings and encoding)
        if let Some((path, job)) = self.tabs[i].editorconfig_job.as_mut() {
            match job.take() {
                Some(props) => {
                    let path = path.clone();
                    self.tabs[i].editorconfig_job = None;
                    self.tabs[i].editorconfig = Some((path, props));
                    self.detect_indent(i);
                }
                None => running = true,
            }
        }
        // Newline index.
        if let Some(job) = self.tabs[i].index_job.as_mut() {
            if job.take().is_some() {
                self.tabs[i].index_job = None;
                self.tabs[i].doc.poll_index();
                self.tabs[i].view.clear_cache();
                if self.tabs[i].filter_again {
                    self.filter_again(i);
                }
                if i == self.active && self.tabs[i].doc.index_error().is_none() {
                    self.drop_flash(STILL_READING);
                }
                if let Some(why) = self.tabs[i].doc.index_error() {
                    // It stays as it is (readable, not editable): line counts can't be guessed.
                    self.tabs[i].notice = Some(Notice {
                        kind: NoticeKind::Error,
                        text: format!(
                            "Slate couldn't read all of this file ({why}), so line numbers and editing are off. \
                             Reload to try again."
                        ),
                        actions: vec![
                            ("Reload".into(), NoticeAction::Reload),
                            ("Dismiss".into(), NoticeAction::Dismiss),
                        ],
                    });
                    self.layout();
                }
                // It may have changed on disk meanwhile (a clean document reloads only once its lines are read).
                self.check_disk();
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
                let id = self.tabs[i].id;
                if task.kind == TaskKind::ReplaceAll && self.replacing.as_ref().is_some_and(|b| b.pending.contains(&id)) {
                    self.replaced_in(i, task, r);
                    return self.tabs.iter().any(|t| t.task.is_some());
                }
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
                let s = &mut self.tabs[i].search;
                s.job = None;
                s.started = None;
                if found.complete {
                    s.found = Some(found);
                    s.found_at = s.key.as_ref().map_or(0, |k| k.4);
                } else {
                    // (no count for this text: not the last one's either)
                    s.found = None;
                }
            } else {
                running = true;
            }
        }
        // Find next / previous through all tabs, on another thread.
        if let Some(f) = self.tabs[i].find_all.as_mut() {
            if let Some(r) = f.job.take() {
                let f = self.tabs[i].find_all.take().unwrap();
                // (not if a text searched or where it started from changed meanwhile)
                let same = |id: u64| {
                    let now = self.tabs.iter().find(|t| t.id == id).map(|t| t.doc.version);
                    now.is_some() && now == f.versions.iter().find(|v| v.0 == id).map(|v| v.1)
                };
                let found_same = r.is_none_or(|(id, ..)| same(id));
                if i == self.active && self.tabs[i].view.sel == f.sel && same(self.tabs[i].id) && found_same {
                    self.found_in_tabs(r);
                }
            } else {
                running = true;
            }
        }
        // Find next / previous, or the search as you type, on another thread.
        if let Some(f) = self.tabs[i].find_job.as_mut() {
            if let Some(r) = f.job.take() {
                let f = self.tabs[i].find_job.take().unwrap();
                // (not if the text or the selection changed meanwhile: the user clicked elsewhere, or typed)
                let tab = &self.tabs[i];
                if i == self.active && tab.doc.version == f.version && tab.view.sel == f.sel {
                    let r = r.map(|(s, e, wrapped)| ((s, e), wrapped));
                    match r {
                        Some(((s, e), _)) if f.live => self.select_match(s, e),
                        _ if f.live => {}
                        r => self.found_next(r),
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
                // The file at that path is a new one now: sources still reading the one before (in this tab's undo,
                // or in text it had that the save didn't move onto the new file) can't be read in a later run.
                let new = saved.rebase.as_ref().map(|r| r.0.clone());
                let target = st.path.as_os_str();
                let at_path = |s: &Source| s.file_path().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(target));
                for t in &self.tabs {
                    for s in t.doc.buffer().sources() {
                        if at_path(s) && new.as_ref().is_none_or(|n| !Arc::ptr_eq(n, s)) {
                            s.mark_gone();
                        }
                    }
                }
                let tab = &mut self.tabs[i];
                if tab.doc.id() != st.doc {
                    // Another document is in this tab now (it was read again meanwhile): what's on disk is newer
                    // than it, which the next look at the disk tells.
                    self.flash("Saved", false);
                    return false;
                }
                let ext = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
                // A new name, or a new kind of name (notes.txt saved as notes.md): pick the language again.
                let renamed = tab.doc.path.as_deref().map(ext) != Some(ext(&st.path));
                let moved = tab.doc.path.as_deref() != Some(st.path.as_path());
                tab.doc.path = Some(st.path.clone());
                tab.canon = Some((st.path.clone(), saved.canon));
                // (a tab of results saved as a file is that file now: a tab like any other, kept in the session)
                if tab.results.take().is_some() {
                    tab.read_only = false;
                    tab.title_override = None;
                    tab.lang_picked = false;
                }
                tab.doc.encoding = st.encoding;
                tab.doc.disk = saved.disk;
                tab.doc.mark_saved_at(st.state);
                // (parts of a damaged file are saved as U+FFFD now, as the user agreed: the file has what the text has)
                tab.doc.bad_units = 0;
                tab.doc.suspect = false;
                tab.seen_disk = None;
                // A big file: the text reads from the saved file from now on (what was typed meanwhile aside).
                if let Some((src, start, nl)) = saved.rebase {
                    if tab.doc.version == st.version {
                        tab.doc.rebase_on(src, start, nl);
                    } else {
                        tab.doc.rebase_after_save(&st.snap, src, start);
                    }
                }
                if renamed {
                    let head = tab.doc.read(0, 4096);
                    let name = Some(st.path.to_string_lossy().into_owned());
                    tab.lang = Lang::detect(name.as_deref(), &head);
                    tab.lang_picked = false;
                }
                if matches!(tab.notice.as_ref().map(|n| n.kind), Some(NoticeKind::Error) | Some(NoticeKind::Warn)) {
                    tab.notice = None;
                }
                if saved.lossy {
                    tab.notice = Some(Notice {
                        kind: NoticeKind::Warn,
                        text: if st.encoding == Encoding::Ansi {
                            "Some characters can't be stored in ANSI and were saved as \"?\".".into()
                        } else {
                            format!("Bytes that weren't text were saved in {} as \"\u{FFFD}\".", st.encoding.label())
                        },
                        actions: vec![
                            ("Save as UTF-8".into(), NoticeAction::SaveUtf8),
                            ("Dismiss".into(), NoticeAction::Dismiss),
                        ],
                    });
                }
                self.settings.add_recent(&st.path);
                super::jumplist::update(self.settings.recent.clone());
                self.session_dirty = true;
                // (a file somewhere new, a new tab's first save: EditorConfig's rule there, which may have come back
                // before the save gave the tab that path)
                if moved {
                    self.detect_indent(i);
                }
                if saved.lossy {
                    // (only when the user said so) The tab keeps the real characters: never close it now, nor the
                    // window it was being saved for.
                    self.cancel_close();
                    self.layout();
                    self.update_title();
                    return false;
                }
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
                } else if !self.shortcuts_saved(i, &st.path) {
                    self.flash("Saved", false);
                }
                self.update_title();
                false
            }
            Err(e) => {
                let closing = self.closing;
                self.cancel_close();
                let msg = match &e {
                    SaveError::Cancelled => {
                        self.flash("Saving was cancelled.", false);
                        return false;
                    }
                    SaveError::Lossy => {
                        // Nothing was written: ask what to do (outside this borrow, it shows a dialog).
                        let tab = &mut self.tabs[i];
                        tab.ask_lossy = Some((st.path.clone(), st.close_after, closing, st.encoding));
                        self.pending.push(Deferred::AskLossy(tab.id));
                        return false;
                    }
                    SaveError::Changed => {
                        self.tabs[i].notice = Some(Notice {
                            kind: NoticeKind::Error,
                            text: format!("Couldn't save: {e}"),
                            actions: vec![
                                ("Reload (lose my changes)".into(), NoticeAction::Reload),
                                ("Dismiss".into(), NoticeAction::Dismiss),
                            ],
                        });
                        self.layout();
                        return false;
                    }
                    SaveError::Outside(now) => {
                        // (told now: saving again keeps the user's version, as after the look's own notice)
                        let tab = &mut self.tabs[i];
                        tab.seen_disk = Some(Some(*now));
                        tab.notice = Some(Notice {
                            kind: NoticeKind::Warn,
                            text: e.to_string(),
                            actions: vec![
                                ("Reload (lose my changes)".into(), NoticeAction::Reload),
                                ("Keep mine".into(), NoticeAction::KeepMine),
                            ],
                        });
                        self.layout();
                        return false;
                    }
                    SaveError::ReadOnly => "This file is read-only, so it couldn't be saved.".to_string(),
                    // (Linux only)
                    SaveError::Owner | SaveError::Access => format!("Couldn't save: {e}"),
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
                    if count == 0 {
                        self.flash(nothing_to_clean(op), false);
                        return;
                    }
                }
                if matches!(task.kind, TaskKind::SortColumn { .. }) && count == 0 {
                    self.flash("The rows are already in that order", false);
                    return;
                }
                let sel = tab.view.sel;
                let new_len = src.len();
                // (line endings: the caret and the view stay on their lines; an offset kept as it was could be
                // inside a CRLF)
                let places = matches!(task.kind, TaskKind::Eol(_)).then(|| (tab.doc.place_of(sel.caret), tab.doc.place_of(tab.view.top)));
                // (bookmarks stay on their lines where the lines stay what they were; elsewhere they go)
                let marks = matches!(task.kind, TaskKind::Eol(_) | TaskKind::ReplaceAll)
                    .then(|| crate::edit::marks_as_lines(&tab.view.bookmarks, &tab.doc));
                let (scope, old_len) = (tab.view.scope, tab.doc.len());
                tab.doc.begin(EditKind::Other, sel);
                tab.doc.replace_all_with(src, nl);
                let keep_place = matches!(task.kind, TaskKind::ReplaceAll | TaskKind::Lines(_) | TaskKind::SortColumn { .. });
                let new_sel = match places {
                    Some((caret, _)) => Sel::at(caret.and_then(|p| tab.doc.offset_of_place(p)).unwrap_or(0)),
                    None if keep_place => Sel::at(sel.caret.min(new_len)),
                    None => Sel::at(0),
                };
                tab.doc.end(new_sel);
                tab.view.sel = new_sel;
                tab.sync_view();
                tab.view.scope = match task.kind {
                    TaskKind::ReplaceAll => scope.map(|(a, b)| (a, (b + new_len).saturating_sub(old_len).max(a))),
                    _ => None,
                };
                tab.view.bookmarks = marks.map_or_else(Vec::new, |l| crate::edit::marks_at_lines(&l, &tab.doc));
                if let TaskKind::Eol(e) = task.kind {
                    tab.doc.eol = e;
                }
                if let Some((_, top)) = places {
                    tab.view.top = top.and_then(|p| tab.doc.offset_of_place(p)).map_or(0, |o| tab.doc.line_start_of(o));
                } else if !keep_place {
                    tab.view.top = 0;
                }
                let whole = task.kind == TaskKind::ReplaceAll && self.change_every_whole.take_if(|t| *t == tab.id).is_some();
                let msg = match task.kind {
                    TaskKind::Format(_) => "Formatted".to_string(),
                    TaskKind::Minify(_) => "Minified".to_string(),
                    TaskKind::ReplaceAll if whole => format!("Replaced {} (every whole word: in a text this big, in code and names too)", plural(count, "match", "matches")),
                    TaskKind::ReplaceAll => format!("Replaced {}", plural(count, "match", "matches")),
                    TaskKind::Eol(e) => format!("Line endings changed to {} ({})", e.short(), plural(count, "line", "lines")),
                    TaskKind::Lines(op) => lines_done(op, count),
                    TaskKind::SortColumn { col, d, header, .. } => {
                        let name = header.then(|| crate::edit::csv_column_name(&crate::edit::csv_header(&tab.doc, d).0, d, col)).flatten();
                        crate::edit::csv_sorted(count, col, name.as_deref(), header)
                    }
                    TaskKind::Validate(_) | TaskKind::Print | TaskKind::Filter(_) | TaskKind::Refilter | TaskKind::Hash { .. } => String::new(),
                };
                if matches!(task.kind, TaskKind::Format(_)) {
                    // (now indented by the formatter's own rule)
                    self.detect_indent(i);
                }
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
                let ls = tab.doc.line_start_of(off);
                // in characters, like the status bar (bytes for a line too long to count)
                let col = if off - ls <= 4 << 20 { bytecount::num_chars(&tab.doc.read(ls, off)) as u64 + 1 } else { off - ls + 1 };
                let at = match line {
                    Some(l) => format!(" (line {}, column {})", group(l + 1), group(col)),
                    None => String::new(),
                };
                // (nested deeper than Slate follows isn't invalid)
                let deep = msg == crate::core::json::TOO_DEEP;
                let verb = match task.kind {
                    TaskKind::Validate(_) if deep => format!("Couldn't check this {what}"),
                    TaskKind::Validate(_) => format!("Not valid {what}"),
                    _ if deep => format!("Couldn't format this {what}"),
                    _ => format!("Couldn't format: not valid {what}"),
                };
                tab.notice = Some(Notice {
                    kind: NoticeKind::Error,
                    text: format!("{verb}: {msg}{at}"),
                    actions: vec![("Show me".into(), NoticeAction::GoTo(off)), ("Dismiss".into(), NoticeAction::Dismiss)],
                });
                // (the status bar's message is older: "Valid JSON" from the check before can't stay beside this)
                self.flash = None;
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
                self.flash = None;
                self.layout();
            }
            TaskResult::Done(m) => self.flash(m, false),
            TaskResult::Hash(d) => {
                let TaskKind::Hash { from, to } = task.kind else { return };
                let tab = &self.tabs[i];
                let what = if from == 0 && to == tab.doc.len() && tab.doc.version == task.version {
                    tab.title()
                } else {
                    format!("the selection ({})", crate::edit::plural(to - from, "byte", "bytes"))
                };
                self.hash = Some((what, crate::convert::hex(&d)));
                self.pending.push(Deferred::Hash);
            }
            TaskResult::Filtered { src, nl, found, query, matcher } => {
                let TaskKind::Filter(invert) = task.kind else { return };
                self.show_filtered(i, crate::edit::FilterStep { matcher, invert, query }, src, nl, found);
            }
            TaskResult::Refiltered { src, nl, map, found } => self.refiltered(i, src, nl, map, found),
            TaskResult::Cancelled => self.flash("Cancelled", false),
        }
    }

    /// The lines a filter of tab `i` kept, in a tab of their own after it (or what it found when no line is kept).
    fn show_filtered(&mut self, i: usize, step: crate::edit::FilterStep, src: Arc<Source>, nl: u64, found: crate::core::search::Filtered) {
        let (invert, query) = (step.invert, step.query.as_str());
        let q: String = if query.chars().count() > 40 { query.chars().take(39).collect::<String>() + "…" } else { query.to_string() };
        if found.map.lines == 0 {
            self.flash(if invert { format!("Every line has “{q}”") } else { format!("No line has “{q}”") }, false);
            return;
        }
        // (lines of a tab of lines: of the tab those came from)
        let source = &self.tabs[i];
        let (origin, map, mut steps) = match &source.filter {
            Some(f) => (f.source, found.map.through(&f.map), f.steps.clone()),
            None => (source.id, found.map, Vec::new()),
        };
        steps.push(step.clone());
        let (lang, encoding, eol) = (source.lang, source.doc.encoding, source.doc.eol);
        let from = self.tabs.iter().find(|t| t.id == origin);
        let name = from.map_or_else(|| "the file".to_string(), |t| t.title());
        let total = from.and_then(|t| crate::edit::text_lines(&t.doc));
        let mut doc = Document::from_buffer(crate::core::buffer::Buffer::from_source(src, nl));
        doc.encoding = encoding;
        doc.eol = eol;
        let lines = map.lines;
        let k = self.add_tab(doc);
        let tab = self.tabs.remove(k);
        let at = (i + 1).min(self.tabs.len());
        self.tabs.insert(at, tab);
        let tab = &mut self.tabs[at];
        tab.untitled = 0;
        tab.lang = lang;
        tab.lang_picked = true;
        tab.filter = Some(FilterOf { source: origin, map: Arc::new(map), steps });
        tab.title_override = Some(if invert { format!("not “{q}” in {name}") } else { format!("“{q}” in {name}") });
        tab.notice = Some(Notice {
            kind: NoticeKind::Info,
            text: crate::edit::filtered_note(lines, total, &name, &q, invert, found.cut),
            actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
        });
        self.activate(at);
        self.layout();
        self.update_title();
    }

    /// Tab `i`'s file was read again (a log that grew): its tabs of matching lines are made again from it, each on
    /// another thread (`refiltered`), once its lines are counted. A tab busy with something else is left as it is.
    fn filter_again(&mut self, i: usize) {
        if !self.tabs[i].doc.is_ready() {
            self.tabs[i].filter_again = true;
            return;
        }
        self.tabs[i].filter_again = false;
        let id = self.tabs[i].id;
        let snap = self.tabs[i].doc.snapshot();
        let notify = self.notify.clone();
        for t in self.tabs.iter_mut().filter(|t| t.task.is_none()) {
            let Some(f) = t.filter.as_ref().filter(|f| f.source == id) else { continue };
            let (steps, snap) = (f.steps.clone(), snap.clone());
            let job = Job::spawn(snap.len(), notify.clone(), move |ctx| {
                guarded(&snap, || match crate::edit::filter_again(&steps, snap.clone(), ctx) {
                    Ok((src, nl, map, found)) => TaskResult::Refiltered { src, nl, map, found },
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err(e) => TaskResult::Failed(format!("Couldn't go through the lines again: {e}")),
                })
            });
            t.task = Some(Task { kind: TaskKind::Refilter, job, version: t.doc.version });
        }
        self.timer(TIMER_JOBS, 100);
    }

    /// The lines of tab `i` (of matching lines) made again: the caret stays on its line, or at the end if it was
    /// there (following a log).
    fn refiltered(&mut self, i: usize, src: Arc<Source>, nl: u64, map: crate::core::search::LineMap, found: crate::core::search::Filtered) {
        let Some(f) = self.tabs[i].filter.as_ref() else { return };
        let (origin, last) = (f.source, f.steps.last().cloned());
        let from = self.tabs.iter().find(|t| t.id == origin);
        let name = from.map_or_else(|| "the file".to_string(), |t| t.title());
        let total = from.and_then(|t| crate::edit::text_lines(&t.doc));
        let tab = &mut self.tabs[i];
        let at_end = tab.view.sel.caret >= tab.doc.len() && !tab.doc.is_empty();
        let (line, top) = (tab.doc.line_of(tab.view.sel.caret).unwrap_or(0), tab.doc.line_of(tab.view.top).unwrap_or(0));
        let (encoding, eol) = (tab.doc.encoding, tab.doc.eol);
        let mut doc = Document::from_buffer(crate::core::buffer::Buffer::from_source(src, nl));
        doc.encoding = encoding;
        doc.eol = eol;
        let lines = map.lines;
        tab.doc = doc;
        tab.search = Search::default();
        tab.view.forget_text();
        tab.view.bookmarks.clear();
        let len = tab.doc.len();
        let at = if at_end { len } else { tab.doc.line_start(line).unwrap_or(len) };
        tab.view.sel = Sel::at(at);
        tab.view.top = tab.doc.line_start(top).unwrap_or(0).min(at);
        if let Some(f) = tab.filter.as_mut() {
            f.map = Arc::new(map);
        }
        if let (Some(step), Some(n)) = (last, tab.notice.as_mut()) {
            let q: String = if step.query.chars().count() > 40 { step.query.chars().take(39).collect::<String>() + "…" } else { step.query.clone() };
            n.text = crate::edit::filtered_note(lines, total, &name, &q, step.invert, found.cut);
        }
        if at_end && i == self.active {
            self.reveal_caret(false);
        }
        self.invalidate();
    }

    /// Edit → Find in all tabs (issue #15): the lines the find bar's search matches in every tab (not tabs of matching
    /// lines or of results), listed in a tab of results (`many::in_tabs`).
    fn list_in_tabs(&mut self) {
        use crate::core::many;
        let Some(m) = self.search_for_many("Find in all tabs") else { return };
        let q = self.find.query.text.clone();
        let srcs: Vec<many::TabSource> = self
            .tabs
            .iter_mut()
            .filter(|t| t.filter.is_none() && t.results.is_none() && t.load_job.is_none() && t.restore.is_none())
            .map(|t| {
                let name = match t.doc.path.as_deref().and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()) {
                    Some(dir) => format!("{}  —  {}", t.title(), dir.display()),
                    None => t.title(),
                };
                (t.id, t.doc.version, name, t.doc.snapshot())
            })
            .collect();
        let job = Job::spawn(srcs.len() as u64, self.notify.clone(), move |ctx| Ok(many::in_tabs(&m, &q, srcs, ctx)));
        self.start_many(job, format!("Found: {}", short_query(&self.find.query.text)), "every tab".into(), false);
    }

    /// Edit → Find in files (issue #15): the lines the find bar's search matches in the text files in the folder of the
    /// tab's file and the folders in it (`many::in_files`: not version control, dependencies or build output; not
    /// binary files or ones over 32 MB), listed in a tab of results. On another thread: Esc stops it.
    fn find_in_files(&mut self) {
        use crate::core::many;
        // (a name given relative to where Slate started: in that folder)
        let root = self.tab().doc.path.as_deref().and_then(|p| p.parent()).map(|d| std::path::absolute(if d.as_os_str().is_empty() { Path::new(".") } else { d }).unwrap_or_else(|_| d.to_path_buf()));
        let Some(root) = root else {
            self.flash("Find in files looks in the folder of the tab's file: save this one first, or go to a tab with a file.", false);
            return;
        };
        let Some(m) = self.search_for_many("Find in files") else { return };
        let q = self.find.query.text.clone();
        let what = format!("the files in {}", root_name(self.tab().doc.path.as_deref()));
        let job = Job::spawn(1, self.notify.clone(), move |ctx| Ok(many::in_files_results(&m, &q, &root, ctx)));
        self.start_many(job, format!("Found in files: {}", short_query(&self.find.query.text)), what, true);
    }

    /// Find in all tabs or in files started: kept running (`many::Running`), the status bar saying so until it ends.
    fn start_many(&mut self, job: Job<Result<crate::core::many::Results, String>>, title: String, what: String, files: bool) {
        let from = (self.tab().id, self.tab().doc.version);
        // (no message of its own: the status bar's count shows at once, `status_message`)
        self.flash = None;
        self.many = Some(crate::core::many::Running { job, title, what, files, stopping: false, from });
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
    }

    /// The find bar's search for `what` (Find in all tabs, …), or None with the find bar opened to type one.
    fn search_for_many(&mut self, what: &str) -> Option<Arc<Matcher>> {
        if self.many.is_some() {
            self.flash("Still looking (Esc stops it)", true);
            return None;
        }
        match (self.find.matcher.clone(), self.find.error.clone()) {
            (Some(m), _) => {
                crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
                Some(m)
            }
            (None, Some(e)) => {
                self.flash(e, true);
                None
            }
            (None, None) => {
                self.exec(Cmd::Find);
                self.flash(format!("Type what to find, then Edit → {what}"), false);
                None
            }
        }
    }

    /// Esc: Find in all tabs or in files stops (what it found so far is still listed). False if none runs.
    fn stop_many(&mut self) -> bool {
        let Some(r) = self.many.as_mut() else { return false };
        if !r.stop() {
            return false;
        }
        let s = r.status();
        self.flash(s, false);
        true
    }

    /// Find in all tabs or in files finished: its results in a tab of their own (read only, not kept in the session),
    /// shown unless the user went on with something else meanwhile. While it runs, the status bar counts.
    fn poll_many(&mut self) -> bool {
        let Some(running) = self.many.as_mut() else { return false };
        let Some(r) = running.job.take() else {
            // (the status bar's count, unless a newer message is up)
            if self.flash.as_ref().is_none_or(|(_, at, _)| at.elapsed() >= std::time::Duration::from_millis(1500)) {
                self.invalidate();
            }
            return true;
        };
        let Some(running) = self.many.take() else { return false };
        match r {
            Ok(results) if results.stopped && results.lines == 0 => self.flash("Stopped: nothing found until then", false),
            Ok(results) => {
                let (stopped, lines) = (results.stopped, results.lines);
                let shown = (self.tab().id, self.tab().doc.version);
                let mut doc = Document::from_text(results.text.as_bytes());
                doc.mark_saved();
                let was = self.active;
                let k = self.add_tab(doc);
                let tab = &mut self.tabs[k];
                tab.title_override = Some(running.title.clone());
                tab.untitled = 0;
                tab.read_only = true;
                tab.lang = Lang::Results;
                tab.lang_picked = true;
                tab.results = Some(Arc::new(results));
                let found = crate::edit::plural(lines, "line", "lines");
                if running.may_show(shown) {
                    self.activate(k);
                    self.flash(if stopped { format!("Stopped: {found} found until then") } else { "Enter or a double-click on a line goes there".to_string() }, false);
                } else {
                    // (the user went on meanwhile: the tab waits behind)
                    self.activate(was);
                    self.flash(format!("Found {found}: tab “{}”", running.title), false);
                }
            }
            Err(e) => self.flash(e, false),
        }
        self.invalidate();
        false
    }

    /// Edit → Replace in all tabs (issue #15): the find bar's search replaced with its replacement in every tab that can
    /// be edited, each as its own Replace all (one undo step in each tab). Only with the replace box on screen: the
    /// replacement is what it shows.
    fn replace_in_tabs(&mut self) {
        self.replaced_check();
        if self.replacing.is_some() {
            self.flash("Still replacing (Esc stops it in the tab shown)", true);
            return;
        }
        if !(self.find.open && self.find.mode == BarMode::Replace) {
            self.open_find(BarMode::Replace);
            FindBar::focus(if self.find.matcher.is_some() { self.find.replace_edit } else { self.find.find_edit });
            self.flash("Type what to find and its replacement, then Edit → Replace in all tabs", false);
            return;
        }
        let Some(m) = self.search_for_many("Replace in all tabs") else { return };
        let typed = FindBar::text_of(self.find.replace_edit);
        crate::edit::remember(&mut self.settings.replace_history, &typed);
        let mut b = crate::core::many::ReplaceInTabs { replacement: typed.clone(), ..Default::default() };
        let mut ids = Vec::new();
        for t in &self.tabs {
            if t.read_only || t.filter.is_some() || t.results.is_some() {
                b.read_only += 1;
            } else if t.load_job.is_some() || t.restore.is_some() || !t.doc.is_ready() {
                b.opening += 1;
            } else if t.task.is_some() {
                b.busy += 1;
            } else {
                ids.push(t.id);
            }
        }
        if ids.is_empty() {
            self.flash(format!("No tab to replace in: {}", b.message().trim_start_matches("No matches to replace in any tab; ")), false);
            return;
        }
        b.pending = ids.clone();
        self.replacing = Some(b);
        for id in ids {
            let Some(i) = self.tabs.iter().position(|t| t.id == id) else { continue };
            let replacement = crate::edit::replacement(&typed, self.find.query.regex, self.tabs[i].doc.eol.as_bytes());
            self.replace_with = Some((m.clone(), replacement));
            self.start_task_in(i, TaskKind::ReplaceAll);
            // (a task that didn't start: done at once)
            if self.replace_with.take().is_some() || self.tabs[i].task.is_none() {
                if let Some(b) = self.replacing.as_mut() {
                    b.finished(id, 0, true);
                }
            }
        }
        self.flash("Replacing in every tab…", false);
        self.replaced_check();
    }

    /// One tab's Replace all of Replace in all tabs finished: its text changed (no message of its own).
    fn replaced_in(&mut self, i: usize, task: Task, r: TaskResult) {
        let id = self.tabs[i].id;
        let (count, applied) = match &r {
            TaskResult::Content { count, .. } => (*count, self.tabs[i].doc.version == task.version),
            _ => (0, true),
        };
        if (count > 0 && applied) || !matches!(r, TaskResult::Content { .. }) {
            self.finish_task(i, task, r);
        }
        if let Some(b) = self.replacing.as_mut() {
            b.finished(id, count, applied);
        }
        self.replaced_check();
    }

    /// Replace in all tabs: once every tab's is done (or its tab closed), what it did.
    pub fn replaced_check(&mut self) {
        let Some(b) = self.replacing.as_mut() else { return };
        let tabs = &self.tabs;
        b.prune(|id| tabs.iter().any(|t| t.id == id && t.task.is_some()));
        if b.done() {
            let msg = b.message();
            self.replacing = None;
            self.flash(msg, false);
        }
    }

    /// Enter or a double-click in a tab of matching lines: the caret's line, in the tab the lines came from. In a tab of
    /// results (Find in all tabs, Find in files): that result's tab or file, at its line.
    fn go_to_source(&mut self) {
        let tab = self.tab();
        if let Some(r) = tab.results.clone() {
            use crate::core::many::Place;
            let Some(place) = tab.doc.line_of(tab.view.sel.caret).and_then(|l| r.place(l).cloned()) else { return };
            match place {
                Place::Tab(id, line) => {
                    let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
                        self.flash("That tab is closed.", true);
                        return;
                    };
                    let moved = r.moved(&Place::Tab(id, line), self.tabs[i].doc.version);
                    self.activate(i);
                    self.tabs[i].goto = Some(Goto::Line(line + 1, None));
                    self.apply_goto(i);
                    if moved {
                        self.flash(format!("The text changed since the search: line {} may be another now", group(line + 1)), false);
                    }
                }
                Place::File(p, line) => {
                    let goto = Some(Goto::Line(line + 1, None));
                    if let Some(i) = self.tabs.iter().position(|t| has_file(t, &p, None)) {
                        self.tabs[i].goto = goto;
                        self.activate(i);
                        self.apply_goto(i);
                    } else if let Some(id) = self.open_path(&p, false, None, None) {
                        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                            t.goto = goto;
                        }
                        self.settle(&[id]);
                        let a = self.active;
                        self.activate(a);
                    }
                }
            }
            self.layout();
            self.update_title();
            return;
        }
        let Some(f) = &tab.filter else { return };
        let Some(line) = tab.doc.line_of(tab.view.sel.caret).and_then(|l| f.map.source(l)) else { return };
        let source = f.source;
        let Some(i) = self.tabs.iter().position(|t| t.id == source) else {
            self.flash("The tab these lines came from is closed.", true);
            return;
        };
        self.activate(i);
        self.tabs[i].goto = Some(Goto::Line(line + 1, None));
        self.apply_goto(i);
        self.layout();
        self.update_title();
    }

    /// View → Show only matching lines: the find bar's search (opened to type one), the lines in a tab of their own.
    fn filter_lines(&mut self, invert: bool) {
        if self.find.matcher.is_none() && self.highlights.is_empty() {
            match self.find.error.clone() {
                Some(e) => self.flash(e, true),
                None => {
                    self.exec(Cmd::Find);
                    self.flash("Type what the lines should have, then Alt+L.", false);
                }
            }
            return;
        }
        if !self.tab().doc.is_ready() {
            self.flash("Still reading the file's lines — try again in a moment.", true);
            return;
        }
        self.start_task(TaskKind::Filter(invert));
    }

    pub fn start_task(&mut self, kind: TaskKind) {
        self.start_task_in(self.active, kind);
    }

    /// `start_task` in tab `i` (Replace in all tabs: another than the one shown, its editing checked before).
    fn start_task_in(&mut self, i: usize, kind: TaskKind) {
        let notify = self.notify.clone();
        let changes = !matches!(kind, TaskKind::Validate(_) | TaskKind::Filter(_) | TaskKind::Hash { .. });
        if changes && i == self.active && !self.editable() {
            return;
        }
        let query = self.find.query.text.clone();
        let indent = vec![b' '; self.settings.json_indent as usize];
        // (a word's spelling changed everywhere: its own search and replacement, not the find bar's)
        let given = if kind == TaskKind::ReplaceAll { self.replace_with.take() } else { None };
        let typed = FindBar::text_of(self.find.replace_edit);
        let mut replacement = crate::edit::replacement(&typed, self.find.query.regex, self.tabs[i].doc.eol.as_bytes());
        if kind == TaskKind::ReplaceAll && given.is_none() {
            crate::edit::remember(&mut self.settings.replace_history, &typed);
            crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
        }
        let mut scope = self.tabs[i].view.scope;
        let mut matcher = self.find.matcher.clone();
        if let Some((m, r)) = given {
            (matcher, replacement, scope) = (Some(m), r, None);
        }
        let mut query = query;
        // (with highlighted searches, the lines with any of them, or with this one)
        if matches!(kind, TaskKind::Filter(_)) && !self.highlights.is_empty() {
            let mut qs: Vec<crate::core::search::Query> = self.highlights.iter().map(|(q, _)| q.clone()).collect();
            if matcher.is_some() && !qs.contains(&self.find.query) {
                qs.push(self.find.query.clone());
            }
            match Matcher::any(&qs) {
                Ok(m) => matcher = Some(Arc::new(m)),
                Err(e) => {
                    self.flash(e, true);
                    return;
                }
            }
            query = qs.iter().map(|q| q.text.as_str()).collect::<Vec<_>>().join("” or “");
        }
        let tab = &mut self.tabs[i];
        if tab.task.is_some() {
            self.flash("Still working on the previous task (Esc cancels it).", true);
            return;
        }
        let snap = tab.doc.snapshot();
        let version = tab.doc.version;
        let eol = tab.doc.eol.as_bytes().to_vec();
        let len = snap.len();
        let line_count = tab.doc.line_count().unwrap_or(0);
        let job = match kind {
            // (started by `start_print`, `filter_again`)
            TaskKind::Print | TaskKind::Refilter => return,
            TaskKind::Hash { from, to } => Job::spawn(to - from, notify, move |ctx| guarded(&snap, || {
                match crate::convert::sha256_of(&snap, from, to, ctx) {
                    Some(d) => TaskResult::Hash(d),
                    None => TaskResult::Cancelled,
                }
            })),
            TaskKind::Filter(invert) => {
                let Some(m) = matcher else { return };
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut sink = match Sink::new(len) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    match m.filter_lines_to(&snap, invert, &mut sink, &mut idx, ctx) {
                        Ok(found) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Filtered { src, nl, found, query, matcher: m.clone() },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the lines: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        Err(e) => TaskResult::Failed(format!("Couldn't go through the lines: {e}")),
                    }
                }))
            }
            TaskKind::Format(f) | TaskKind::Minify(f) => {
                let pretty = matches!(kind, TaskKind::Format(_));
                let mode = if pretty { JsonMode::Pretty } else { JsonMode::Minify };
                let hint = if pretty { len.saturating_mul(3) } else { len };
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut sink = match Sink::limited(hint, crate::edit::transform_max(len)) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let (w, ix) = (Some(&mut sink as &mut dyn Write), Some(&mut idx));
                    let r = match f {
                        Fmt::Json => json::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg)),
                        Fmt::Xml => xml::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg)),
                        Fmt::Csv(_) => return TaskResult::Failed("CSV and TSV files aren't formatted.".into()),
                    };
                    match r {
                        Ok(()) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Content { src, nl, count: 0 },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        // (the result, not the text: a full disk, or far too big)
                        Err((_, msg)) if msg.starts_with(crate::core::json::WRITE_FAILED) => {
                            match crate::edit::format_stopped(f == Fmt::Xml, &snap, msg, ctx) {
                                Ok(msg) => TaskResult::Failed(msg),
                                Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                            }
                        }
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
                    Fmt::Csv(d) => csv::check(&snap, d, ctx).map(|s| crate::edit::csv_valid(d, s)).map_err(|e| (e.offset, e.msg)),
                };
                match r {
                    Ok(msg) => TaskResult::Valid(msg),
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                }
            })),
            TaskKind::Lines(op) => {
                if len > LINES_MAX || line_count > lines::COUNT_MAX {
                    self.flash("Sorting and cleaning up lines works for files up to 512 MB and 32 million lines.", true);
                    return;
                }
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut buf = Vec::new();
                    let text = snap.slice(0, snap.len(), &mut buf);
                    if ctx.cancelled() {
                        return TaskResult::Cancelled;
                    }
                    let mut sink = match Sink::new(text.len() as u64) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let count = match lines::apply_to(op, text, &mut |piece| {
                        idx.push(piece);
                        sink.write_all(piece)
                    }) {
                        Ok(count) => count,
                        Err(e) => return TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    };
                    drop(buf);
                    match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count },
                        Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    }
                }))
            }
            TaskKind::SortColumn { col, desc, d, header } => {
                if len > LINES_MAX || line_count > lines::COUNT_MAX {
                    self.flash("Sorting works for files up to 512 MB and 32 million lines.", true);
                    return;
                }
                let day_first = crate::core::os::day_first();
                Job::spawn(len, notify, move |ctx| guarded(&snap, || {
                    let mut buf = Vec::new();
                    let text = snap.slice(0, snap.len(), &mut buf);
                    if ctx.cancelled() {
                        return TaskResult::Cancelled;
                    }
                    let mut sink = match Sink::new(text.len() as u64) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let sorted = match csv::sort_by_column(text, d, col, desc, day_first, header, &mut |piece| {
                        idx.push(piece);
                        sink.write_all(piece)
                    }) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    };
                    drop(buf);
                    match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count: sorted.rows },
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
                    match m.replace_all_to(&snap, &replacement, &mut sink, &mut idx, scope, ctx) {
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

    /// Prints the tab's text (or its selection) on a started printer, on another thread; `pages`: which (None: all).
    pub fn start_print(&mut self, printer: super::print::Printer, selection: bool, pages: Option<Vec<(u32, u32)>>) {
        let notify = self.notify.clone();
        let s = &self.settings;
        let tab = &mut self.tabs[self.active];
        let sel = tab.view.sel;
        let snap = tab.doc.snapshot();
        let (from, to) = if selection && !sel.is_empty() { (sel.start(), sel.end()) } else { (0, snap.len()) };
        let line = tab.doc.line_of(from).unwrap_or(0) + 1;
        let line_start = tab.doc.line_start_of(from);
        let style = print_style(s, tab, to);
        let lang = tab.lang;
        let job = Job::spawn(to - from, notify, move |ctx| {
            let before = snap.read_errors();
            let state = crate::print::state_at(&snap, lang, line_start, from);
            let text = crate::print::Text { snap: &snap, from, to, line, lang, state };
            let mut going = |done: u64| {
                ctx.set(done);
                !ctx.cancelled()
            };
            match super::print::print(printer, &text, &style, pages.as_deref(), &mut going) {
                Ok(Some(_)) if snap.read_errors() != before => {
                    TaskResult::Failed("Printed, but part of the file couldn't be read (was it changed or removed?): the printout misses it.".into())
                }
                Ok(Some(n)) => TaskResult::Done(super::print::done_message(n)),
                Ok(None) => TaskResult::Cancelled,
                Err(m) => TaskResult::Failed(format!("Couldn't print: {m}")),
            }
        });
        tab.task = Some(Task { kind: TaskKind::Print, job, version: tab.doc.version });
        self.timer(TIMER_JOBS, 100);
        self.invalidate();
    }

    // ---- editing ----

    /// Whether the active document can be edited right now (tells the user why not).
    pub fn editable(&mut self) -> bool {
        let tab = self.tab();
        let why = if tab.filter.is_some() {
            Some("These are lines of another tab, to look through: Enter or a double-click goes to one there.")
        } else if tab.read_only {
            Some("This tab is read only: click “Read only” on the right (or File → Read only) to edit it.")
        } else if tab.restore.is_some() {
            Some("This tab from last time isn't back yet (see the note above the text).")
        } else if tab.load_job.is_some() {
            Some("Still opening the file…")
        } else if !tab.doc.is_ready() && tab.doc.index_error().is_some() {
            Some("Part of this file couldn't be read, so it can't be edited. Reload it to try again.")
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

    /// After any change to the text (it ends a column selection: what keeps one sets it again after this).
    pub fn after_edit(&mut self) {
        self.tab_mut().view.block = None;
        if self.flash.as_ref().is_some_and(|f| f.0 == "Saved") {
            self.flash = None;
        }
        let tab = &mut self.tabs[self.active];
        tab.discard = false;
        tab.sync_view();
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.search.job = None;
        if !self.find.open {
            tab.search.found = None;
            tab.search.key = None;
        }
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
        self.tab_mut().view.block = None;
        // (cursors that met as they moved become one)
        let view = &mut self.tabs[self.active].view;
        if !view.carets.is_empty() {
            let mut main = view.sel;
            view.carets.tidy(&mut main);
            view.sel = main;
        }
        self.tab_mut().doc.seal();
        self.reveal_caret(false);
        self.restart_caret();
        self.invalidate();
    }

    pub fn type_text(&mut self, s: &str) {
        if s.is_empty() || !self.editable() {
            return;
        }
        if let Some(b) = self.tab().view.block {
            self.block_edit(b, crate::edit::BlockEdit::Put(vec![s.as_bytes()]));
            return;
        }
        if !self.tab().view.carets.is_empty() {
            self.multi_edit(crate::edit::MultiEdit::Put(vec![s.as_bytes()]), EditKind::Typing);
            return;
        }
        let overtype = self.overtype;
        let tab = &mut self.tabs[self.active];
        let sel = tab.view.sel;
        // Closing a block that Enter indented: the bracket lines up with the line that opened it (in code and JSON;
        // in text, data and makefiles a bracket is just typed).
        let code = !matches!(tab.lang, Lang::Plain | Lang::Log | Lang::Markdown | Lang::Csv | Lang::CsvSemi | Lang::Tsv) && !needs_tabs(tab);
        if (s == "}" || s == "]") && code && !overtype {
            if let Some(new) = editor::close_bracket(&mut tab.doc, sel, s.as_bytes()[0]) {
                tab.view.sel = new;
                self.after_edit();
                return;
            }
        }
        // Each word is its own undo step.
        if s.starts_with(char::is_whitespace) && sel.is_empty() {
            let before = tab.doc.prev_char(sel.caret);
            let prev = tab.doc.read(before, sel.caret);
            if !prev.is_empty() && !prev.iter().all(|b| b.is_ascii_whitespace()) {
                tab.doc.seal();
            }
        }
        // (Typing over a selection starts a typing run too, so undo takes back the selection and what replaced it
        // in one step.)
        tab.view.sel = if overtype {
            editor::overtype(&mut tab.doc, sel, s.as_bytes())
        } else {
            editor::replace_selection(&mut tab.doc, sel, s.as_bytes(), EditKind::Typing)
        };
        self.after_edit();
    }

    /// An edit at every cursor (multiple cursors: `crate::edit::multi_edit`).
    fn multi_edit(&mut self, what: crate::edit::MultiEdit, kind: EditKind) {
        if !self.editable() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let mut main = tab.view.sel;
        crate::edit::multi_edit(&mut tab.doc, &mut main, &mut tab.view.carets, what, kind);
        tab.view.sel = main;
        self.after_edit();
    }

    /// Keys with multiple cursors: Backspace, Delete, Enter and Tab edit at each; the arrows, Home and End move them
    /// all (`move_carets`: Left and Right the others, the main one then moving as always; `after_move` tidies); Esc
    /// leaves only the main one, as do the keys that don't work with several (Page Up, Ctrl+Home…). None: the key goes
    /// on as usual.
    fn carets_key(&mut self, k: VIRTUAL_KEY, m: &Mods) -> Option<bool> {
        use crate::edit::{CaretMove, MultiEdit};
        let how = match k {
            VK_LEFT => Some(if m.ctrl { CaretMove::WordLeft } else { CaretMove::Left }),
            VK_RIGHT => Some(if m.ctrl { CaretMove::WordRight } else { CaretMove::Right }),
            VK_UP if !m.ctrl => Some(CaretMove::Up),
            VK_DOWN if !m.ctrl => Some(CaretMove::Down),
            VK_HOME if !m.ctrl => Some(CaretMove::Home),
            VK_END if !m.ctrl => Some(CaretMove::End),
            _ => None,
        };
        if let Some(how) = how {
            let tab_size = self.settings.tab_size;
            let tab = &mut self.tabs[self.active];
            let mut main = tab.view.sel;
            if crate::edit::move_carets(&tab.doc, &mut main, &mut tab.view.carets, how, m.shift, tab_size) {
                // (Up, Down, Home and End: the main one by the others' rule, so they all go the same way)
                tab.view.set_caret(main.caret, m.shift);
                tab.view.want_x = None;
                self.after_move();
                return Some(true);
            }
            return None;
        }
        let unit = self.indent_now().unit();
        let edit = match k {
            VK_BACK => Some((MultiEdit::Delete { back: true, word: m.ctrl }, if m.ctrl { EditKind::Other } else { EditKind::Backspace })),
            VK_DELETE => Some((MultiEdit::Delete { back: false, word: m.ctrl }, if m.ctrl { EditKind::Other } else { EditKind::DeleteForward })),
            VK_RETURN if !m.ctrl && self.tab().filter.is_none() => Some((MultiEdit::Newline, EditKind::Other)),
            VK_TAB if !m.ctrl && !m.shift => Some((MultiEdit::Put(vec![&unit[..]]), EditKind::Typing)),
            _ => None,
        };
        if let Some((what, kind)) = edit {
            self.multi_edit(what, kind);
            return Some(true);
        }
        if k == VK_ESCAPE {
            self.tab_mut().view.carets.clear();
            self.invalidate();
            return Some(true);
        }
        // (Ctrl+Up / Down only scroll: the cursors stay, as with the wheel)
        if matches!(k, VK_PRIOR | VK_NEXT | VK_HOME | VK_END | VK_TAB | VK_RETURN) {
            self.tab_mut().view.carets.clear();
        }
        None
    }

    /// Edit → Multiple cursors' commands (`crate::edit`); with the setting off, only how to turn it on.
    fn caret_cmd(&mut self, cmd: Cmd) {
        if !self.settings.multi_cursor {
            self.flash("Multiple cursors are off: Edit → Multiple cursors → Use multiple cursors turns them on.", false);
            return;
        }
        let tab_size = self.settings.tab_size;
        let tab = &mut self.tabs[self.active];
        tab.view.block = None;
        tab.doc.seal();
        let (doc, view) = (&tab.doc, &mut tab.view);
        let mut main = view.sel;
        let c = &mut view.carets;
        let r: Result<Option<String>, &str> = match cmd {
            Cmd::AddCaretAbove => crate::edit::add_caret_line(doc, &mut main, c, false, tab_size).map(|_| None),
            Cmd::AddCaretBelow => crate::edit::add_caret_line(doc, &mut main, c, true, tab_size).map(|_| None),
            Cmd::AddNextMatch => crate::edit::add_next_match(doc, &mut main, c, false).map(|_| None),
            Cmd::SkipMatch => crate::edit::add_next_match(doc, &mut main, c, true).map(|_| None),
            Cmd::AllMatches => crate::edit::all_matches(doc, &mut main, c).map(|n| Some(format!("{} cursors", group(n as u64)))),
            Cmd::LineStartCarets => crate::edit::carets_on_lines(doc, &mut main, c, false).map(|_| None),
            Cmd::LineEndCarets => crate::edit::carets_on_lines(doc, &mut main, c, true).map(|_| None),
            _ => {
                crate::edit::remove_last_caret(&mut main, c);
                Ok(None)
            }
        };
        view.sel = main;
        view.want_x = None;
        match r {
            Ok(Some(msg)) => self.flash(msg, false),
            Ok(None) => {}
            Err(e) => self.flash(e, true),
        }
        self.reveal_caret(false);
        self.restart_caret();
        self.invalidate();
    }

    // ---- tabs in the title bar ----

    /// The mouse over caption button `b` (None: over none of them now): its background. Windows says when the mouse
    /// leaves them (TME_NONCLIENT).
    pub fn caption_hover(&mut self, b: Option<u8>) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
        if b.is_some() {
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: self.hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
        } else if matches!(self.down, Hit::Caption(_)) {
            self.down = Hit::None;
        }
        let h = b.map_or(Hit::None, Hit::Caption);
        if (b.is_some() || matches!(self.hover, Hit::Caption(_))) && self.hover != h {
            self.hover = h;
            self.invalidate();
        }
    }

    pub fn caption_press(&mut self, b: u8) {
        self.down = Hit::Caption(b);
        self.hover = Hit::Caption(b);
        self.invalidate();
    }

    /// Let go over caption button `b`: what it does (SC_MINIMIZE, SC_MAXIMIZE or SC_RESTORE, SC_CLOSE, for
    /// WM_SYSCOMMAND), when it's the one pressed. Test mode only notes it (`print:asked`): it would show the hidden
    /// window, or end the test.
    pub fn caption_release(&mut self, b: Option<u8>) -> Option<usize> {
        let pressed = std::mem::replace(&mut self.down, Hit::None);
        // (the next move over the buttons shows which one the mouse is on again)
        self.hover = Hit::None;
        self.invalidate();
        let b = b.filter(|&b| pressed == Hit::Caption(b))?;
        use windows::Win32::UI::WindowsAndMessaging::{IsZoomed, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE};
        let zoomed = unsafe { IsZoomed(self.hwnd) }.as_bool();
        let (cmd, name) = match b {
            0 => (SC_MINIMIZE, "Minimize"),
            1 if zoomed => (SC_RESTORE, "Restore"),
            1 => (SC_MAXIMIZE, "Maximize"),
            _ => (SC_CLOSE, "Close"),
        };
        if win::SCRIPTED.with(|s| s.borrow_mut().as_mut().map(|s| s.asked.push(name.into())).is_some()) {
            return None;
        }
        Some(cmd as usize)
    }

    // ---- spell check ----

    /// A word spelled wrong at (`x`, `y`) in the text (client DIPs): what it may have been meant to be, with the word
    /// noted for the menu's commands (`spell_word`). None: not one there.
    pub fn spelling_at(&mut self, x: f32, y: f32) -> Option<Vec<String>> {
        self.spell_word = None;
        // (F7's word is for this menu only, whatever it shows)
        let menu_word = self.menu_word.take();
        let tab = self.tab();
        if !self.settings.spell_check || tab.read_only || tab.filter.is_some() || tab.results.is_some() || !matches!(self.speller, Spelling::Ready(_)) {
            return None;
        }
        // (F7's word: by where it is, as the rows on screen may not be laid out for it yet)
        let (pos, words) = match menu_word {
            Some(p) => (p, self.with_view(|v, cx| v.words_at(cx, p).1)),
            None => {
                let (pos, _) = self.with_view(|v, cx| v.pos_at(cx, x, y));
                (pos, self.with_view(|v, cx| v.spell_words(cx)))
            }
        };
        let (start, end, word) = words.into_iter().find(|&(a, b, _)| a <= pos && pos <= b)?;
        let Spelling::Ready(s) = &mut self.speller else { return None };
        if !s.wrong(&word) {
            return None;
        }
        let suggestions = s.suggest(&word);
        let tab = self.tab();
        self.spell_word = Some(SpellWord { tab: tab.id, version: tab.doc.version, start, end, word, suggestions: suggestions.clone() });
        Some(suggestions)
    }

    /// Puts suggestion `i` in place of the word the menu was for (while the text is as it was).
    fn spell_replace(&mut self, i: usize) {
        let Some(w) = self.spell_word.take() else { return };
        let Some(s) = w.suggestions.get(i) else { return };
        if self.tab().id != w.tab || self.tab().doc.version != w.version || !self.editable() {
            return;
        }
        let tab = self.tab_mut();
        tab.doc.seal();
        tab.view.sel = editor::replace_selection(&mut tab.doc, Sel { anchor: w.start, caret: w.end }, s.as_bytes(), EditKind::Other);
        tab.doc.seal();
        self.after_edit();
    }

    /// Go back's places follow the caret (`edit::Nav`): after each message, and each test-mode step.
    pub fn note_spot(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let now = crate::edit::Spot { tab: tab.id, pos: tab.view.sel.caret };
        // (a screenful of lines, as Page Down moves: not a jump)
        self.nav.note_in(now, &tab.doc, tab.view.rows.len());
    }

    /// Edit → Go back (`back`) or Go forward (issue #11): to the tab and place the caret jumped from.
    fn go_nav(&mut self, back: bool) {
        let tab = self.tab();
        let now = crate::edit::Spot { tab: tab.id, pos: tab.view.sel.caret };
        let ids: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        match self.nav.go(back, now, |t| ids.contains(&t)) {
            Some(s) => {
                if let Some(k) = self.tabs.iter().position(|t| t.id == s.tab) {
                    self.activate(k);
                }
                let doc = &self.tab().doc;
                let pos = editor::char_start(doc, s.pos.min(doc.len()));
                self.go_to(pos, true);
            }
            None => self.flash(if back { "Nothing to go back to" } else { "Nothing to go forward to" }, false),
        }
    }

    /// Notes where each tab's file was left (Settings → Reopen files where you left them), as Slate closes.
    pub fn remember_places(&mut self) {
        let places: Vec<(PathBuf, u64, u64)> = self
            .tabs
            .iter()
            .filter(|t| t.doc.disk.is_some() && t.title_override.is_none())
            .filter_map(|t| Some((t.doc.path.clone()?, t.view.sel.caret, t.view.top)))
            .collect();
        for (p, caret, top) in places.into_iter().rev() {
            self.settings.remember_place(&p, caret, top);
        }
    }

    /// File → Open selected file or link (issue #7; `at`: the text's menu's place, see `links::target_in`): a file in a
    /// tab of its own (at its line, for `name:12`), opened as Open does it; a web link in the browser. Never runs a
    /// program.
    fn open_selection(&mut self, at: Option<u64>) {
        use crate::links::Target;
        let tab = self.tab();
        match crate::links::target_in(&tab.doc, tab.view.sel, at, false) {
            Ok(Target::Link(url)) => {
                win::shell_open(self.hwnd, std::ffi::OsStr::new(&url), &format!("open link {url}"));
                self.flash(format!("Opening {url} in your browser"), false);
            }
            Ok(Target::File(p, at)) => {
                let goto = at.map(|(l, c)| Goto::Line(l, Some(c)));
                if let Some(i) = self.tabs.iter().position(|t| has_file(t, &p, None)) {
                    if goto.is_some() {
                        self.tabs[i].goto = goto;
                    }
                    self.activate(i);
                } else if let Some(id) = self.open_path(&p, false, None, None) {
                    // (no line asked for: where the file was left, which `open_path` set, stays)
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id).filter(|_| goto.is_some()) {
                        t.goto = goto;
                    }
                    self.settle(&[id]);
                    let a = self.active;
                    self.activate(a);
                }
            }
            Err(e) => self.flash(e, false),
        }
    }

    /// The spell check menu's Change every … to: suggestion `i` in place of the word everywhere the spell check would
    /// mark it (`spell::places_of`: not in code, a file name or a link, which it passes over), one undo step. A text
    /// over `CHANGE_EVERY_MAX` (too much to look through here) gets a whole-word Replace all on a job. Issue #12.
    fn spell_replace_all(&mut self, i: usize) {
        let Some(w) = self.spell_word.take() else { return };
        let Some(s) = w.suggestions.get(i).cloned() else { return };
        if self.tab().id != w.tab || !self.editable() {
            return;
        }
        let len = self.tab().doc.len();
        if len <= crate::spell::CHANGE_EVERY_MAX {
            let places = self.with_view(|v, cx| crate::spell::places_of(&w.word, len, |p| v.words_at(cx, p)));
            if places.is_empty() {
                return;
            }
            let tab = self.tab_mut();
            tab.doc.seal();
            tab.view.sel = crate::edit::replace_places(&mut tab.doc, &places, s.as_bytes(), tab.view.sel);
            tab.doc.seal();
            self.after_edit();
            self.flash(format!("Changed “{}” to “{s}” in {}", w.word, crate::edit::plural(places.len() as u64, "place", "places")), false);
            return;
        }
        // (its message says it was every whole word this time)
        self.change_every_whole = Some(self.tab().id);
        let q = search::Query { text: w.word.clone(), match_case: true, whole_word: true, regex: false };
        match Matcher::new(&q) {
            Ok(m) => {
                self.replace_with = Some((Arc::new(m), s.as_bytes().to_vec()));
                self.start_task(TaskKind::ReplaceAll);
                // (not taken if the task didn't start)
                self.replace_with = None;
            }
            Err(e) => self.flash(e, true),
        }
    }

    /// F7: the next word spelled wrong after the caret (round to the start), selected, with the spell check menu
    /// open at it (issue #12). Looks through `LOOK` bytes at a time.
    fn next_misspelling(&mut self) {
        let tab = self.tab();
        if !self.settings.spell_check {
            self.flash("Spell check is off (View → Spell check)", false);
            return;
        }
        if tab.read_only || tab.results.is_some() {
            self.flash("This tab is read only: F7 finds words to correct", false);
            return;
        }
        if tab.binary || tab.filter.is_some() || !crate::spell::checks(tab.lang) {
            let what = if tab.binary {
                "binary files".to_string()
            } else if tab.filter.is_some() {
                "a tab of matching lines".to_string()
            } else {
                format!("{} files", tab.lang.label())
            };
            self.flash(format!("Slate doesn't check the spelling in {what}"), false);
            return;
        }
        match &self.speller {
            Spelling::Ready(_) => {}
            Spelling::Unavailable => {
                self.flash("Windows has no dictionary for its language, so there's no spell check", true);
                return;
            }
            _ => {
                // (F7 again by itself once it's ready)
                self.f7_wanted = true;
                self.flash("Spell check is starting…", false);
                return;
            }
        }
        // (F7 again while one goes round in this text: it goes on; else one starts at the selection's end)
        let (id, version, from) = (tab.id, tab.doc.version, tab.view.sel.end());
        if !self.f7.is_some_and(|r| r.tab == id && r.version == version) {
            self.f7 = Some(crate::spell::Round::new(id, version, from));
        }
        // (test mode: all at once, its windows have no timers running)
        if win::testing() {
            while self.f7_slice() {}
        } else if self.f7_slice() {
            self.timer(TIMER_F7, 1);
        }
    }

    /// A part of F7's round (`spell::Round`, `F7_TIME` long): the word it finds selected with its menu, "No words spelled
    /// wrong" back where it began, else how far it is (true: more to look at). Another tab or an edit ends it.
    fn f7_slice(&mut self) -> bool {
        use crate::spell::Step;
        let Some(mut round) = self.f7.take() else { return false };
        let tab = self.tab();
        if tab.id != round.tab || tab.doc.version != round.version {
            return false;
        }
        let len = tab.doc.len();
        let mut sp = match std::mem::replace(&mut self.speller, Spelling::Unavailable) {
            Spelling::Ready(sp) => sp,
            other => {
                self.speller = other;
                return false;
            }
        };
        let until = std::time::Instant::now() + crate::spell::F7_TIME;
        let step = self.with_view(|v, cx| round.step(&mut sp, len, until, |p| v.words_at(cx, p)));
        self.speller = Spelling::Ready(sp);
        match step {
            Step::Found(a, b) => {
                let tab = self.tab_mut();
                tab.view.sel = Sel { anchor: a, caret: b };
                tab.doc.seal();
                self.after_move();
                self.reveal_caret(true);
                self.menu_word = Some(a);
                self.context_menu_at_caret();
                false
            }
            Step::Done => {
                self.flash("No words spelled wrong", false);
                false
            }
            Step::More(pct) => {
                self.f7 = Some(round);
                self.flash(format!("Looking for words spelled wrong… {pct}% (Esc stops)"), false);
                true
            }
        }
    }

    /// Esc before anything else: F7 going round, Find in all tabs or in files, or the tab shown's part of Replace in all
    /// tabs (whose replace box is open) stops. True if one did.
    fn stop_first(&mut self) -> bool {
        if self.f7.take().is_some() {
            self.kill_timer(TIMER_F7);
            self.flash("Stopped looking for words spelled wrong", false);
            return true;
        }
        if self.stop_many() {
            return true;
        }
        if let Some(t) = self.tab().task.as_ref().filter(|_| self.replacing.is_some()) {
            t.job.cancel();
            self.flash("Cancelling…", false);
            return true;
        }
        false
    }

    // ---- input methods (`ime`) ----

    /// An input method starts composing: from now on what it composes shows at the caret, and a selection goes, as
    /// typing over it would take it. False when the text can't be typed in (the reason shows): it's dropped.
    pub fn ime_start(&mut self) -> bool {
        if !self.editable() {
            return false;
        }
        let tab = self.tab_mut();
        if tab.view.block.is_none() && !tab.view.sel.is_empty() {
            tab.view.sel = editor::replace_selection(&mut tab.doc, tab.view.sel, b"", EditKind::Other);
            self.after_edit();
        }
        self.tab_mut().view.set_composing(Some(Composing::default()));
        self.wake_caret();
        self.invalidate();
        true
    }

    /// What the input method has now: a `result` to type (as keys type it), and what it's composing.
    pub fn ime_composition(&mut self, result: Option<&str>, comp: Composing) {
        let composing = self.tab().view.composing.is_some();
        if let Some(r) = result {
            self.tab_mut().view.set_composing(None);
            self.type_text(r);
        }
        // (only while one shows here: not after `ime_finish` typed it)
        if composing {
            self.tab_mut().view.set_composing(Some(comp));
        }
        self.wake_caret();
        self.invalidate();
    }

    /// The input method is done (what it chose was typed already).
    pub fn ime_end(&mut self) {
        for t in &mut self.tabs {
            t.view.set_composing(None);
        }
        self.invalidate();
    }

    /// Ends a composition by typing what shows of it (before a click moves the caret). True when there was one: the
    /// input method's own copy is to be dropped then (`ime::cancel`).
    pub fn ime_finish(&mut self) -> bool {
        let Some(c) = self.tab().view.composing.clone() else { return false };
        self.tab_mut().view.set_composing(None);
        self.type_text(&c.text);
        self.invalidate();
        true
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

    /// Alt pressed and released on its own: the menu bar takes the keyboard, File highlighted (like Notepad's).
    /// Pressing Alt again leaves it.
    pub fn toggle_menu_bar(&mut self) {
        self.menu_armed = if self.menu_armed.is_some() { None } else { Some(0) };
        self.menu_bar_changed();
        self.invalidate();
    }

    pub fn disarm_menu_bar(&mut self) {
        if self.menu_armed.take().is_some() {
            self.menu_bar_changed();
            self.invalidate();
        }
    }

    /// Whether the menu bar shows: always, unless Settings → Hide the menu bar is on (issue #10); then while Alt has given
    /// it the keyboard or one of its menus is open.
    pub fn menu_bar_shown(&self) -> bool {
        !self.settings.hide_menu_bar || self.menu_armed.is_some() || self.menu_open.is_some()
    }

    /// The window laid out again when the menu bar shows or hides (`menu_bar_shown`).
    fn menu_bar_changed(&mut self) {
        if (self.r_menu.h > 0.0) != self.menu_bar_shown() {
            self.layout();
            self.invalidate();
        }
    }

    /// A key while the menu bar has the keyboard: a menu's letter opens it, Left/Right move along the bar, Enter or
    /// Down open the highlighted menu, Space the window menu. Anything else leaves the menu bar and then works as
    /// usual (None).
    fn menu_bar_key(&mut self, vk: u16, m: &Mods) -> Option<bool> {
        let cur = self.menu_armed?;
        let n = MENU_TITLES.len();
        let k = VIRTUAL_KEY(vk);
        let open = |a: &mut App, i: usize| {
            a.menu_armed = None;
            a.drop_typed_char();
            a.pending.push(Deferred::Menu(i));
            Some(true)
        };
        match k {
            // Holding a modifier keeps the menu bar; Alt again leaves it (WM_SYSCOMMAND toggles it off).
            VK_SHIFT | VK_CONTROL | VK_MENU | VK_LSHIFT | VK_RSHIFT | VK_LCONTROL | VK_RCONTROL | VK_LMENU | VK_RMENU => {
                Some(false)
            }
            VK_LEFT | VK_RIGHT => {
                self.menu_armed = Some(if k == VK_LEFT { (cur + n - 1) % n } else { (cur + 1) % n });
                self.invalidate();
                Some(true)
            }
            VK_RETURN | VK_DOWN | VK_UP => open(self, cur),
            VK_ESCAPE | VK_F10 => {
                self.disarm_menu_bar();
                Some(true)
            }
            VK_SPACE => {
                // The window menu (Restore, Move, Close...), as Alt+Space opens it.
                self.disarm_menu_bar();
                self.drop_typed_char();
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        self.hwnd,
                        windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND,
                        windows::Win32::Foundation::WPARAM(windows::Win32::UI::WindowsAndMessaging::SC_KEYMENU as usize),
                        windows::Win32::Foundation::LPARAM(b' ' as isize),
                    );
                }
                Some(true)
            }
            _ => match menu_for_letter(vk) {
                Some(i) if !m.ctrl && !m.alt => open(self, i),
                _ => {
                    self.disarm_menu_bar();
                    None
                }
            },
        }
    }

    /// A key that opens a menu typed a character too (TranslateMessage has queued it): drop it, or the menu would
    /// take it as a choice (Alt, F would pick "Show in folder").
    fn drop_typed_char(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_CHAR, WM_SYSCHAR};
        unsafe {
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, self.hwnd, WM_CHAR, WM_CHAR, PM_REMOVE);
            let _ = PeekMessageW(&mut msg, self.hwnd, WM_SYSCHAR, WM_SYSCHAR, PM_REMOVE);
        }
    }

    /// The command a key gives: the user's keys first (`crate::shortcuts`), then Slate's own (`global_key` for
    /// `anywhere`, also in the find bar's boxes; `editor_key` in the text), unless the user gave that command or those
    /// keys to something else.
    fn key_cmd(&self, vk: u16, m: &Mods, anywhere: bool) -> Option<Cmd> {
        // (Edit → Multiple cursors' keys only while it's on: off, they do what they did before)
        let off = |id: &str| !self.settings.multi_cursor && crate::shortcuts::CARET_COMMANDS.contains(&id);
        let combo = super::commands::combo(vk, m);
        if let Some(c) = combo.and_then(|c| self.keymap.command_for(&c)) {
            return if c.anywhere == anywhere && !off(c.id) { super::commands::cmd_of(c.id) } else { None };
        }
        let own = if anywhere { global_key(vk, m) } else { editor_key(vk, m) }?;
        match super::commands::id_of(own) {
            Some(id) if self.keymap.keys_of(id).is_some() || off(id) => None,
            _ => Some(own),
        }
    }

    /// The shortcuts file was saved: its keys count from now, and what couldn't be used is said on its tab. Returns
    /// whether it was that file.
    fn shortcuts_saved(&mut self, i: usize, path: &Path) -> bool {
        if !path.as_os_str().eq_ignore_ascii_case(crate::shortcuts::path().as_os_str()) {
            return false;
        }
        let user = crate::shortcuts::read();
        self.keymap = crate::shortcuts::Keymap::new(&user);
        if user.problems.is_empty() {
            self.flash("Saved — Slate uses these shortcuts now", false);
        } else {
            self.tabs[i].notice = Some(Notice {
                kind: NoticeKind::Warn,
                text: format!("Saved, but not all of it can be used: {}", user.problems.join("; ")),
                actions: vec![("Dismiss".into(), NoticeAction::Dismiss)],
            });
            self.layout();
        }
        true
    }

    /// Key presses in the text area. Returns whether the key was used.
    pub fn on_key(&mut self, vk: u16) -> bool {
        self.hide_tip();
        self.wake_caret();
        let m = mods();
        if VIRTUAL_KEY(vk) == VK_MENU {
            self.alt_dragged = false;
        }
        if let Some(used) = self.menu_bar_key(vk, &m) {
            return used;
        }
        if let Some(cmd) = self.key_cmd(vk, &m, true) {
            self.pending.push(Deferred::Cmd(cmd));
            return true;
        }
        let k = VIRTUAL_KEY(vk);
        // (Alt and a menu's letter opens it, unless the user gave those keys to a command: theirs wins, and the menu
        // is still Alt, then the letter, or F10)
        let mine = super::commands::combo(vk, &m).is_some_and(|c| self.keymap.taken(&c));
        if m.alt && !m.ctrl && !m.shift && !mine {
            if let Some(i) = menu_for_letter(vk) {
                self.drop_typed_char();
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
        if self.block_key(k, &m) {
            self.tab_mut().view.carets.clear();
            return true;
        }
        if let Some(cmd) = self.key_cmd(vk, &m, false) {
            self.pending.push(Deferred::Cmd(cmd));
            return true;
        }
        if m.alt {
            return false;
        }
        if !self.tab().view.carets.is_empty() {
            if let Some(used) = self.carets_key(k, &m) {
                return used;
            }
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
                    editor::next_cluster(&tab.doc, sel.caret)
                } else {
                    editor::prev_cluster(&tab.doc, sel.caret)
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
                if self.tab().filter.is_some() || self.tab().results.is_some() {
                    self.go_to_source();
                } else if self.editable() {
                    let unit = self.indent_now().unit();
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
            VK_INSERT if !m.ctrl && !m.shift => self.exec(Cmd::ToggleOvertype),
            VK_ESCAPE => {
                // Find in all tabs or in files stops first (what it found is still listed: the find bar its search
                // was typed in is usually open); closing and deselecting come next: a save is cancelled only by an
                // Esc meant for nothing else.
                if self.stop_first() {
                } else if self.find.open {
                    self.close_find();
                } else if let Some(t) = &self.tab().task {
                    t.job.cancel();
                } else if self.tab().notice.is_some() {
                    self.tab_mut().notice = None;
                    self.layout();
                    self.invalidate();
                } else if !self.tab().view.sel.is_empty() {
                    let tab = self.tab_mut();
                    let c = tab.view.sel.caret;
                    tab.view.sel = Sel::at(c);
                    self.invalidate();
                } else if let Some(s) = &self.tab().save {
                    s.job.cancel();
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
        let ind = self.indent_now();
        let tab_size = self.settings.tab_size;
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        let multi = !sel.is_empty() && tab.doc.line_start_of(sel.start()) != tab.doc.line_start_of(sel.end().saturating_sub(1).max(sel.start()));
        if shift || multi {
            match editor::indent_lines(&mut tab.doc, sel, ind, tab_size, shift) {
                Ok(s) => tab.view.sel = s,
                Err(why) => {
                    self.flash(why, true);
                    return;
                }
            }
        } else {
            let text = editor::tab_text(&tab.doc, sel.start(), ind, tab_size);
            tab.doc.seal();
            tab.view.sel = editor::replace_selection(&mut tab.doc, sel, &text, EditKind::Other);
        }
        self.after_edit();
    }

    fn context_menu_at_caret(&mut self) {
        self.menu_at_caret = true;
        let p = self.with_view(|v, cx| v.caret_point(cx));
        let (x, y) = p.map(|(x, y)| (x, y + self.style.row_h)).unwrap_or((self.r_edit.x + 40.0, self.r_edit.y + 40.0));
        self.pending.push(Deferred::ContextMenu(x, y));
    }

    pub fn select_match(&mut self, s: u64, e: u64) {
        self.find.last_match = Some((s, e));
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
        if !self.tab().view.carets.is_empty() {
            let tab = self.tab();
            let Some(text) = crate::edit::multi_text(&tab.doc, tab.view.sel, &tab.view.carets) else {
                self.flash("Nothing is selected at the cursors (or it's more than 16 MB).", true);
                return false;
            };
            if !win::set_clipboard(self.hwnd, &text) {
                self.flash(CLIPBOARD_BUSY, true);
                return false;
            }
            self.line_clip = None;
            return true;
        }
        if let Some(b) = self.tab().view.block {
            let tab = self.tab();
            let text = match crate::edit::block_text(&tab.doc, &b, self.settings.tab_size, tab.doc.eol.as_bytes()) {
                Ok(t) => t,
                Err(e) => {
                    self.flash(e, true);
                    return false;
                }
            };
            if !win::set_clipboard(self.hwnd, &text) {
                self.flash(CLIPBOARD_BUSY, true);
                return false;
            }
            self.line_clip = None;
            return true;
        }
        let (a, b) = self.copy_range();
        if a == b {
            return false;
        }
        let text = self.tab().doc.read(a, b);
        if !win::set_clipboard(self.hwnd, &text) {
            self.flash(CLIPBOARD_BUSY, true);
            return false;
        }
        // A whole line (nothing was selected): pasting it puts it back as a line.
        self.line_clip = self.tab().view.sel.is_empty().then(win::clipboard_sequence);
        true
    }

    pub fn cut(&mut self) {
        if !self.editable() || !self.copy() {
            return;
        }
        if !self.tab().view.carets.is_empty() {
            self.multi_edit(crate::edit::MultiEdit::Put(vec![b""]), EditKind::Other);
            return;
        }
        if let Some(b) = self.tab().view.block {
            if b.cols().0 < b.cols().1 {
                self.block_edit(b, crate::edit::BlockEdit::Delete { back: false });
            }
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
        if !self.tab().view.carets.is_empty() {
            // (a line each when it has as many as there are cursors, else all of it at each)
            let text = editor::normalize_eols(&text, self.tab().doc.eol.as_bytes());
            let n = self.tab().view.carets.count();
            self.tab_mut().doc.seal();
            self.multi_edit(crate::edit::MultiEdit::Put(crate::edit::paste_parts(&text, n)), EditKind::Other);
            self.tab_mut().doc.seal();
            return;
        }
        if let Some(b) = self.tab().view.block {
            // into every line: a line of the text each when it has as many as the selection, else all of it
            let text = editor::normalize_eols(&text, b"\n");
            let text = text.strip_suffix(b"\n").unwrap_or(&text);
            let lines: Vec<&[u8]> = text.split(|&c| c == b'\n').collect();
            let (first, last) = b.lines();
            let parts = if lines.len() as u64 == last - first + 1 { lines } else { vec![text] };
            self.block_edit(b, crate::edit::BlockEdit::Put(parts));
            return;
        }
        // A line copied with nothing selected goes in as a line, above the caret's (like VS Code).
        let line = self.line_clip.is_some() && self.line_clip == Some(win::clipboard_sequence());
        let i = self.active;
        let tab = self.tab_mut();
        let mut text = editor::normalize_eols(&text, tab.doc.eol.as_bytes());
        // Pasted into an empty new tab: color it like what it looks like (JSON, XML, a script...).
        let guess = tab.doc.is_empty() && tab.doc.path.is_none() && tab.lang == Lang::Plain && !tab.lang_picked;
        tab.doc.seal();
        let sel = tab.view.sel;
        if line && sel.is_empty() {
            if !text.ends_with(b"\n") {
                text.extend_from_slice(tab.doc.eol.as_bytes());
            }
            let at = tab.doc.line_start_of(sel.caret);
            let new = Sel::at(sel.caret + text.len() as u64);
            tab.doc.begin(EditKind::Other, sel);
            tab.doc.insert(at, &text);
            tab.doc.end(new);
            tab.view.sel = new;
        } else {
            tab.view.sel = editor::replace_selection(&mut tab.doc, sel, &text, EditKind::Other);
        }
        tab.doc.seal();
        if guess {
            tab.lang = Lang::detect(None, &text[..text.len().min(4096)]);
            self.detect_indent(i);
        }
        self.after_edit();
    }

    // ---- find bar ----

    pub fn open_find(&mut self, mode: BarMode) {
        let again = self.find.open && self.find.mode != BarMode::GoTo;
        self.find.open = true;
        self.find.mode = mode;
        let sel = self.tab().view.sel;
        let selected = (!sel.is_empty()).then(|| (sel.start(), sel.end()));
        // (Ctrl+F again while the search's match is selected: what the user selected stays)
        if mode != BarMode::GoTo && !(again && selected.is_some() && selected == self.find.last_match) {
            self.find.sel_before = selected;
        }
        let tab = self.tab();
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
        crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
        self.find.history_at = None;
        if self.tab_mut().view.scope.take().is_some() {
            self.tab_mut().search = Search::default();
        }
        self.layout();
        win::set_focus(self.hwnd);
        self.restart_caret();
        self.invalidate();
    }

    /// A press in the text that closes the find bar was let go (`close_find_on_up`): the bar closes once a double-click
    /// can't come any more (`TIMER_FIND_CLOSE`; another press in the text puts it off), so nothing moves under the
    /// mouse while the clicks go on.
    fn close_find_after_click(&mut self) {
        let Some(id) = self.close_find_on_up.take() else { return };
        self.close_find_later = Some(id);
        self.timer(TIMER_FIND_CLOSE, unsafe { GetDoubleClickTime() }.clamp(100, 2000));
    }

    /// `TIMER_FIND_CLOSE`: the clicks are over. The bar goes (unless the keyboard went back to it), and the text stays
    /// about where it is on screen, scrolled back as many rows as the bar was tall (as far as there are rows above).
    fn close_find_now(&mut self) {
        self.kill_timer(TIMER_FIND_CLOSE);
        let Some(id) = self.close_find_later.take() else { return };
        if !self.find.open || self.in_find_bar() || self.tabs.get(self.active).is_none_or(|t| t.id != id) {
            return;
        }
        let rows = (self.find.height() / self.style.row_h.max(1.0)).round() as i64;
        self.close_find();
        if rows > 0 {
            self.with_view(|v, cx| v.scroll_rows(cx, -rows));
        }
        self.invalidate();
    }

    /// The find text or options changed.
    pub fn on_find_changed(&mut self) {
        let text = FindBar::text_of(self.find.find_edit);
        let changed = text != self.find.query.text;
        self.find.query.text = text;
        self.find.history_at = None;
        self.find.compile();
        if changed {
            self.clear_search_flash();
            self.live_search();
        }
        self.schedule_count();
        self.invalidate();
    }

    /// The search changed: "No results" and the like were about the old one.
    fn clear_search_flash(&mut self) {
        if self.flash_search {
            self.flash = None;
            self.flash_search = false;
        }
    }

    /// As you type, select the first match at or after where the search started.
    fn live_search(&mut self) {
        let Some(m) = self.find.matcher.clone() else { return };
        let (a, b) = self.search_range();
        let origin = self.find.origin.unwrap_or(self.tab().view.sel.start()).clamp(a, b);
        let len = self.tab().doc.len();
        if len > m.sync_limit() {
            self.find_async(m, origin, true, true);
            return;
        }
        let doc = &self.tab().doc;
        let inside = |r: Option<(u64, u64)>| r.filter(|x| x.1 <= b);
        let r = inside(m.find_fwd(doc, origin, b, None)).or_else(|| inside(m.find_fwd(doc, a, origin, None)));
        if let Some((s, e)) = r {
            self.select_match(s, e);
        }
    }

    /// `live`: the search as you type (see `FindJob`).
    fn find_async(&mut self, m: Arc<Matcher>, from: u64, forward: bool, live: bool) {
        let notify = self.notify.clone();
        let (a, b) = self.search_range();
        let tab = self.tab_mut();
        let snap = tab.doc.snapshot();
        let (version, sel) = (tab.doc.version, tab.view.sel);
        let job = Job::spawn(snap.len(), notify, move |ctx| {
            let inside = |r: Option<(u64, u64)>| r.filter(|x| x.1 <= b);
            if forward {
                inside(m.find_fwd(&snap, from, b, Some(ctx))).map(|(s, e)| (s, e, false)).or_else(|| {
                    inside(m.find_fwd(&snap, a, from, Some(ctx))).map(|(s, e)| (s, e, true))
                })
            } else {
                m.find_back(&snap, a, from, Some(ctx)).map(|(s, e)| (s, e, false)).or_else(|| {
                    inside(m.find_back(&snap, from, b, Some(ctx))).map(|(s, e)| (s, e, true))
                })
            }
        });
        tab.find_job = Some(FindJob { job, version, sel, live });
        self.timer(TIMER_JOBS, 100);
    }

    /// Shows what Find next / previous found (the match, and whether it wrapped around), or that it found nothing.
    fn found_next(&mut self, r: Option<((u64, u64), bool)>) {
        match r {
            Some(((s, e), wrapped)) => {
                self.select_match(s, e);
                self.find.origin = Some(s);
                if wrapped {
                    self.flash_search("Search wrapped around", false);
                }
            }
            None => self.flash_search("No results", true),
        }
    }

    /// The match before `from` from a finished count of this search in this text (exact, and at once; for a
    /// regex in a big document the search itself can only guess where a match before the caret starts). None if
    /// there's no such count; Some(None) if it found no match there, nor wrapping around.
    fn counted_previous(&self, from: u64) -> Option<Option<((u64, u64), bool)>> {
        let tab = self.tab();
        let q = &self.find.query;
        let key = (q.text.clone(), q.match_case, q.whole_word, q.regex, tab.doc.version);
        let f = tab.search.found.as_ref().filter(|f| f.complete && f.positions.len() as u64 == f.count)?;
        if tab.search.key.as_ref() != Some(&key) || tab.search.found_at != tab.doc.version {
            return None;
        }
        // (the last that ends at or before `from`, else, wrapping around, the last one if it starts at or after it)
        let k = f.positions.partition_point(|p| p.1 <= from);
        Some(match k {
            0 => f.positions.last().filter(|p| p.0 >= from).map(|&p| (p, true)),
            k => Some((f.positions[k - 1], false)),
        })
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.find.query.text.is_empty() {
            self.open_find(BarMode::Find);
            return;
        }
        let Some(m) = self.find.matcher.clone() else {
            self.flash_search(self.find.error.clone().unwrap_or_default(), true);
            return;
        };
        crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
        let (a, b) = self.search_range();
        let tab = self.tab();
        let sel = tab.view.sel;
        let len = tab.doc.len();
        let mut from = (if forward { sel.end() } else { sel.start() }).clamp(a, b);
        // Don't find the same empty match again.
        if sel.is_empty() && forward && from < b && m.is_match_at(&tab.doc, from, from) {
            from = tab.doc.next_char(from);
        }
        self.find.origin = Some(from);
        // (Find in selection keeps to the selection)
        let all = self.find.all_tabs && self.tab().view.scope.is_none() && self.tabs.len() > 1;
        if !forward {
            match self.counted_previous(from) {
                // (through all tabs, only one before the caret here: else the tabs before this one come first)
                Some(r) if !all || matches!(r, Some((_, false))) => {
                    self.found_next(r);
                    return;
                }
                _ => {}
            }
        }
        if all {
            self.find_in_tabs(m, from, forward);
            return;
        }
        if len > m.sync_limit() {
            self.find_async(m, from, forward, false);
            return;
        }
        let doc = &self.tab().doc;
        let inside = |r: Option<(u64, u64)>| r.filter(|x| x.1 <= b);
        let r = if forward {
            inside(m.find_fwd(doc, from, b, None)).map(|x| (x, false)).or_else(|| inside(m.find_fwd(doc, a, from, None)).map(|x| (x, true)))
        } else {
            m.find_back(doc, a, from, None).map(|x| (x, false)).or_else(|| inside(m.find_back(doc, from, b, None)).map(|x| (x, true)))
        };
        self.found_next(r);
    }

    /// Find next or previous with All tabs on: from `from` to this tab's end (or back to its start), then the other
    /// tabs in their order (each from its start, or back from its end), then the rest of this one; the first match
    /// found is selected, in its tab. A tab of matching lines (its lines are another tab's) or one whose text isn't
    /// there yet is passed over. On another thread when there's more text than `Matcher::sync_limit`.
    fn find_in_tabs(&mut self, m: Arc<Matcher>, from: u64, forward: bool) {
        let (n, cur) = (self.tabs.len(), self.active);
        let len = self.tab().doc.len();
        let here = (self.tab().id, self.tab_mut().doc.snapshot());
        let mut parts = vec![(here.0, here.1.clone(), if forward { (from, len) } else { (0, from) }, false)];
        for k in 1..n {
            let t = &mut self.tabs[if forward { (cur + k) % n } else { (cur + n - k) % n }];
            if t.filter.is_none() && t.text_coming().is_none() {
                parts.push((t.id, t.doc.snapshot(), (0, t.doc.len()), false));
            }
        }
        parts.push((here.0, here.1, if forward { (0, from) } else { (from, len) }, true));
        let total: u64 = parts.iter().map(|p| p.2.1 - p.2.0).sum();
        let search = move |ctx: Option<&crate::core::job::Ctx>| {
            for (id, snap, (a, b), wrapped) in &parts {
                let r = if forward { m.find_fwd(snap, *a, *b, ctx) } else { m.find_back(snap, *a, *b, ctx) };
                if let Some((s, e)) = r {
                    return Some((*id, s, e, *wrapped));
                }
                if ctx.is_some_and(|c| c.cancelled()) {
                    return None;
                }
            }
            None
        };
        if total <= self.find.matcher.as_ref().map_or(0, |m| m.sync_limit()) {
            let r = search(None);
            self.found_in_tabs(r);
            return;
        }
        let versions = self.tabs.iter().map(|t| (t.id, t.doc.version)).collect();
        let notify = self.notify.clone();
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        tab.find_all = Some(FindAllJob { job: Job::spawn(total, notify, move |ctx| search(Some(ctx))), versions, sel });
        self.timer(TIMER_JOBS, 100);
    }

    /// Shows what `find_in_tabs` found: the match, in its tab.
    fn found_in_tabs(&mut self, r: Option<(u64, u64, u64, bool)>) {
        let Some((id, s, e, wrapped)) = r else {
            self.flash_search("No results in any tab", true);
            return;
        };
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return };
        let moved = i != self.active;
        self.activate(i);
        self.select_match(s, e);
        self.find.origin = Some(s);
        if moved {
            self.flash_search(format!("Found in {}", self.tab_label(i)), false);
        } else if wrapped {
            self.flash_search("Search wrapped around (all tabs)", false);
        }
    }

    /// The find box's Highlight (Alt+H): the search kept highlighted in every tab in a color of its own, or, when it
    /// is already, no more.
    fn toggle_highlight(&mut self) {
        let Some(m) = self.find.matcher.clone() else {
            self.flash_search("Type the search to highlight first.", true);
            return;
        };
        let q = self.find.query.clone();
        if let Some(i) = self.highlights.iter().position(|(h, _)| *h == q) {
            self.highlights.remove(i);
            self.flash_search(format!("“{}” isn't highlighted any more", q.text), false);
        } else {
            self.highlights.push((q.clone(), m));
            self.flash_search(format!("“{}” is highlighted in every tab (Show only matching lines keeps the lines with any of them)", q.text), false);
        }
        self.invalidate();
    }

    /// Where the find bar searches in the tab shown: its Find in selection range, else the whole text.
    fn search_range(&self) -> (u64, u64) {
        let tab = self.tab();
        let len = tab.doc.len();
        tab.view.scope.map_or((0, len), |(a, b)| (a.min(len), b.min(len).max(a.min(len))))
    }

    /// Find in selection (Alt+S) on or off: on, the find bar searches what's selected (and goes on searching it as
    /// it's edited) until it's closed or this is turned off.
    fn toggle_in_sel(&mut self) {
        let (before, last) = (self.find.sel_before, self.find.last_match);
        let tab = self.tab_mut();
        if tab.view.scope.take().is_none() {
            let sel = tab.view.sel;
            let selected = (!sel.is_empty()).then(|| (sel.start(), sel.end()));
            // (the match searching as you type selected isn't what the user selected)
            let range = if selected.is_some() && selected != last { selected } else { before.or(selected) };
            let Some((a, b)) = range.filter(|r| r.1 <= tab.doc.len()) else {
                self.flash("Select the text to search in first.", true);
                return;
            };
            tab.view.scope = Some((a, b));
            self.find.origin = Some(a);
        }
        self.tab_mut().search = Search::default();
        self.clear_search_flash();
        self.live_search();
        self.schedule_count();
        self.invalidate();
    }

    /// Up and Down in a find bar box: the searches (or replacements) before, newest first; past the newest, what
    /// was typed.
    fn history_step(&mut self, edit: HWND, older: bool) {
        let find = edit == self.find.find_edit;
        let hist = if find { &self.settings.find_history } else { &self.settings.replace_history };
        let typed = FindBar::text_of(edit);
        let at = match (self.find.history_at, older) {
            // (one the box has already is skipped)
            (None, true) => (0..hist.len()).find(|&i| hist[i] != typed),
            (None, false) => return,
            (Some(i), true) => Some((i + 1).min(hist.len().saturating_sub(1))),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        if at.is_none() && self.find.history_at.is_none() {
            return;
        }
        if self.find.history_at.is_none() {
            self.find.history_typed = typed;
        }
        let hist = if find { &self.settings.find_history } else { &self.settings.replace_history };
        let text = match at {
            Some(i) => hist.get(i).cloned().unwrap_or_default(),
            None => self.find.history_typed.clone(),
        };
        FindBar::set_text(edit, &text);
        FindBar::select_all(edit);
        if find {
            self.on_find_changed();
        }
        self.find.history_at = at;
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
        let (a, b) = self.search_range();
        let q = &self.find.query;
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let key = (q.text.clone(), q.match_case, q.whole_word, q.regex, tab.doc.version);
        if tab.search.key.as_ref() == Some(&key) {
            return;
        }
        // (only the text changed: the last count stays shown until this one is in)
        let same_search = tab.search.key.as_ref().is_some_and(|k| k.0 == key.0 && (k.1, k.2, k.3) == (key.1, key.2, key.3));
        tab.search.key = Some(key);
        if !same_search {
            tab.search.found = None;
            tab.search.marks = None;
        }
        tab.search.started = Some(Instant::now());
        let snap = tab.doc.snapshot();
        tab.search.job = Some(Job::spawn(snap.len(), notify, move |ctx| search::count_in(&m, &snap, a, b, ctx)));
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
                // (counted before the last edit: its places may have moved)
                Some(f) if tab.search.found_at != tab.doc.version => (format!("{} found", group(f.count)), false),
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
        let typed = FindBar::text_of(self.find.replace_edit);
        crate::edit::remember(&mut self.settings.replace_history, &typed);
        crate::edit::remember(&mut self.settings.find_history, &self.find.query.text);
        let repl = crate::edit::replacement(&typed, self.find.query.regex, self.tab().doc.eol.as_bytes());
        let tab = self.tab_mut();
        let sel = tab.view.sel;
        if !sel.is_empty() && sel.end() - sel.start() < (16 << 20) {
            // (a match where it is: "foo" as a whole word isn't one inside "foobar")
            if m.is_match_at(&tab.doc, sel.start(), sel.end()) {
                let hs = sel.start().saturating_sub(64);
                let hay = tab.doc.read(hs, (sel.end() + 64).min(tab.doc.len()));
                let mut out = Vec::new();
                m.expand(&hay, (sel.start() - hs) as usize, &repl, &mut out);
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
        // "12,345" or "12.345" is line 12345 (as the hint writes it); only ':' gives a column ("120:5").
        let digits = |s: &str| s.replace([',', '.', '\'', ' ', '\u{a0}', '\u{202f}'], "").parse::<u64>().ok();
        let (line, col) = match text.trim().split_once(':') {
            Some((l, c)) => (digits(l), digits(c)),
            None => (digits(text.trim()), None),
        };
        let Some(line) = line else {
            // (JSON and XML: a path, `data[3].name`, `/catalog/book[3]`)
            if self.tab().lang.has_structure() {
                return self.go_to_path(text.trim());
            }
            self.flash("Type a line number", true);
            return;
        };
        let Some(pos) = line_col_pos(&self.tab().doc, line, col) else {
            self.flash("Still reading the file's lines — try again in a moment.", true);
            return;
        };
        self.close_find();
        self.go_to(pos, true);
    }

    pub fn find_part(&mut self, p: Part) {
        match p {
            Part::Expand => {
                let mode = if self.find.mode == BarMode::Replace { BarMode::Find } else { BarMode::Replace };
                self.find.mode = mode;
                if mode == BarMode::Find && win::focus() == self.find.replace_edit {
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
                self.clear_search_flash();
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
            Part::InSel => self.toggle_in_sel(),
            Part::Filter => self.filter_lines(false),
            Part::AllTabs => {
                self.find.all_tabs = !self.find.all_tabs;
                self.invalidate();
            }
            Part::Highlight => self.toggle_highlight(),
        }
    }

    /// Keys typed in the find bar's edit boxes. Returns true if handled (the edit box doesn't see it).
    pub fn bar_key(&mut self, edit: HWND, vk: u16) -> bool {
        self.hide_tip();
        let m = mods();
        if let Some(used) = self.menu_bar_key(vk, &m) {
            return used;
        }
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
                // (F7, Find in all tabs or in files, or Replace in all tabs stops first)
                if !self.stop_first() {
                    self.close_find();
                }
                true
            }
            VK_UP | VK_DOWN if !m.ctrl && !m.alt && edit != self.find.goto_edit => {
                self.history_step(edit, k == VK_UP);
                true
            }
            VK_TAB if !m.ctrl => {
                self.find.history_at = None;
                // With the replace box, between the two boxes (Esc goes back to the text, where a habitual second Tab
                // would type over the match selected there).
                if self.find.mode == BarMode::Replace {
                    let next = if edit == self.find.find_edit { self.find.replace_edit } else { self.find.find_edit };
                    FindBar::focus(next);
                    FindBar::select_all(next);
                } else {
                    let close = self.settings.find_close_on_leave;
                    win::set_focus(self.hwnd);
                    self.restart_caret();
                    if close {
                        self.close_find();
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
                    VK_S => Some(Part::InSel),
                    VK_A => Some(Part::AllTabs),
                    VK_H => Some(Part::Highlight),
                    _ => None,
                };
                if let Some(p) = part {
                    self.find_part(p);
                    return true;
                }
                false
            }
            _ => {
                if let Some(cmd) = self.key_cmd(vk, &m, true) {
                    self.pending.push(Deferred::Cmd(cmd));
                    return true;
                }
                false
            }
        }
    }

    // ---- mouse ----

    pub fn on_mouse_down(&mut self, x: f32, y: f32, button: u8) {
        self.disarm_menu_bar();
        self.hide_tip();
        let hit = self.hit(x, y);
        if button == 0 {
            self.close_find_on_up = None;
        }
        // Only a left press is drawn pressed: the others aren't captured, so their release can come anywhere.
        match button {
            0 if std::mem::replace(&mut self.down, hit) != hit => self.invalidate(),
            2 => self.middle_down = hit,
            _ => {}
        }
        let capture = |h: HWND| unsafe {
            SetCapture(h);
        };
        match (button, hit) {
            (0, Hit::Tab(i)) => {
                self.activate(i);
                self.tab_drag = Some((i, x, false));
                capture(self.hwnd);
            }
            // (tabs in the title bar: the strip's empty room is the caption, a double-click there is Windows')
            (0, Hit::TabStrip) if !self.custom_frame => {
                if self.click_count(x, y) == 2 {
                    self.pending.push(Deferred::Cmd(Cmd::NewTab));
                }
            }
            (0, Hit::Menu(i)) => self.pending.push(Deferred::Menu(i)),
            (0, Hit::Text) | (0, Hit::Gutter) => {
                // (the bar closes once the clicks are over, a moment after the button is let go: until then nothing
                // moves under the mouse, so a click, a double-click and a drag are where they were made; this press
                // may be the next click of a run that began in the bar)
                self.kill_timer(TIMER_FIND_CLOSE);
                let more = self.close_find_later.take() == Some(self.tab().id);
                let close = self.settings.find_close_on_leave && (more || self.in_find_bar());
                self.close_find_on_up = close.then(|| self.tab().id);
                win::set_focus(self.hwnd);
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
            (0, Hit::StructBar) => {
                self.struct_bar_down(y);
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

    /// A press on the structure panel's scrollbar: on the thumb it's taken to drag; above or below it, a page up or
    /// down.
    fn struct_bar_down(&mut self, y: f32) {
        let Some((track, thumb, _)) = self.tab().structure.scrollbar(self.r_struct) else { return };
        if y >= thumb.y && y < thumb.bottom() {
            self.struct_drag = Some(y - thumb.y);
        } else {
            let page = (track.h - super::structure::row_h()).max(super::structure::row_h());
            let s = &mut self.tab_mut().structure;
            s.scroll = (s.scroll + if y > thumb.y { page } else { -page }).max(0.0);
        }
        self.invalidate();
    }

    fn click_count(&mut self, x: f32, y: f32) -> u32 {
        let (t, lx, ly, n) = self.last_click;
        let ms = unsafe { GetDoubleClickTime() } as u128;
        let slop = self.px_to_dip(unsafe { GetSystemMetrics(SM_CXDOUBLECLK) }).max(4.0);
        let n = if t.elapsed().as_millis() <= ms && (x - lx).abs() <= slop && (y - ly).abs() <= slop { n % 3 + 1 } else { 1 };
        self.last_click = (Instant::now(), x, y, n);
        n
    }

    /// The column at `x` in the text (as a monospace font has them).
    fn col_at_x(&self, x: f32) -> u32 {
        let geom = self.editor_geom();
        ((x - geom.text_x + self.tab().view.scroll_x) / self.style.char_w.max(1.0)).round().max(0.0) as u32
    }

    fn editor_click(&mut self, x: f32, y: f32, clicks: u32, gutter: bool) {
        let ext = mods().shift;
        let (pos, up) = self.with_view(|v, cx| v.pos_at(cx, x, y));
        self.tab_mut().view.block = None;
        // Ctrl+click with multiple cursors on: a cursor added there, or the one there taken away (in the
        // selection, Ctrl+drag still copies it)
        let m = mods();
        let sel = self.tab().view.sel;
        let in_sel = !sel.is_empty() && pos > sel.start() && pos < sel.end();
        if m.ctrl && !m.alt && !m.shift && !gutter && clicks == 1 && self.settings.multi_cursor && !in_sel {
            let tab = &mut self.tabs[self.active];
            let mut main = tab.view.sel;
            let r = crate::edit::toggle_caret(&tab.doc, &mut main, &mut tab.view.carets, pos);
            tab.view.sel = main;
            tab.view.want_x = None;
            tab.doc.seal();
            if let Err(e) = r {
                self.flash(e, true);
            }
            self.restart_caret();
            self.invalidate();
            return;
        }
        // Ctrl+double-click (triple) with multiple cursors on: the word there (the line) is one more selection
        if m.ctrl && !m.alt && !m.shift && !gutter && clicks >= 2 && self.settings.multi_cursor {
            let tab = &mut self.tabs[self.active];
            let mut main = tab.view.sel;
            let r = crate::edit::add_caret_word(&tab.doc, &mut main, &mut tab.view.carets, pos, clicks >= 3);
            tab.view.sel = main;
            tab.view.want_x = None;
            tab.doc.seal();
            if let Err(e) = r {
                self.flash(e, true);
            }
            self.restart_caret();
            self.invalidate();
            return;
        }
        self.tab_mut().view.carets.clear();
        if mods().alt && !gutter && clicks == 1 {
            // Alt+drag: a column selection (and Alt let go after it doesn't give the menu bar the keyboard)
            self.alt_dragged = true;
            let col = self.col_at_x(x).min(crate::edit::BLOCK_COLS_MAX);
            let tab_size = self.settings.tab_size;
            let tab = self.tab_mut();
            let line = tab.doc.line_of(pos).unwrap_or(0);
            tab.view.block = Some(crate::edit::Block::at(line, col));
            tab.view.sel = Sel::at(crate::edit::pos_at_col(&tab.doc, line, col, tab_size));
            tab.view.drag = Some(DragMode::Block);
            self.restart_caret();
            self.invalidate();
            return;
        }
        if clicks == 2 && !gutter && (self.tab().filter.is_some() || self.tab().results.is_some()) {
            self.tab_mut().view.set_caret(pos, false);
            self.go_to_source();
            return;
        }
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
        } else if !ext && !tab.view.sel.is_empty() && pos > tab.view.sel.start() && pos < tab.view.sel.end() {
            // (in the selection: it may be dragged somewhere else; a click without moving puts the caret there)
            tab.view.drag = Some(DragMode::Text { at: pos, x: x as i32, y: y as i32, moved: false });
        } else {
            tab.view.set_caret(pos, ext);
            tab.view.upstream = up;
            tab.view.drag = Some(DragMode::Chars);
        }
        self.restart_caret();
        self.invalidate();
    }

    /// Drag and drop: the selection moved to `to` (copied, with Ctrl), as one step to undo, and selected there.
    fn drop_text(&mut self, to: u64, copy: bool) {
        let sel = self.tab().view.sel;
        let (a, b) = (sel.start(), sel.end());
        // (onto itself: nothing)
        if sel.is_empty() || (!copy && to >= a && to <= b) || !self.editable() {
            return;
        }
        // (the text is copied: as for Duplicate, not hundreds of MB of it, which a press that slips past the drag
        // threshold in a big selection would otherwise do)
        if b - a > crate::edit::COPY_MAX {
            self.flash(crate::edit::TOO_MUCH_TEXT, true);
            return;
        }
        let tab = self.tab_mut();
        let text = tab.doc.read(a, b);
        let n = text.len() as u64;
        tab.doc.seal();
        tab.doc.begin(EditKind::Other, sel);
        let at = if copy || to < a {
            if !copy {
                tab.doc.delete(a, b);
            }
            tab.doc.insert(to, &text);
            to
        } else {
            tab.doc.insert(to, &text);
            tab.doc.delete(a, b);
            to - n
        };
        let new = Sel::new(at, at + n);
        tab.doc.end(new);
        tab.doc.seal();
        tab.view.sel = new;
        self.after_edit();
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
            self.settings.structure_width = (w0 - (x - x0)).clamp(200.0, super::app::panel_max(self.size.0));
            self.layout();
            self.invalidate();
            return;
        }
        if let Some(grab) = self.struct_drag {
            // the thumb follows the pointer
            if let Some((track, thumb, max)) = self.tab().structure.scrollbar(self.r_struct) {
                let f = (y - grab - track.y) / (track.h - thumb.h).max(1.0);
                self.tab_mut().structure.scroll = f.clamp(0.0, 1.0) * max;
                self.invalidate();
            }
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
            self.tip_follow();
        }
    }

    // ---- tooltips ----

    /// The mouse went onto another part: its tooltip shows after a moment (soon after another one went).
    fn tip_follow(&mut self) {
        self.hide_tip();
        if self.tip_text(self.hover).is_some() {
            let wait = unsafe { GetDoubleClickTime() }.clamp(100, 2000);
            let soon = self.tip_gone.is_some_and(|t| t.elapsed() < Duration::from_millis(wait as u64));
            self.timer(TIMER_TIP, if soon { wait / 5 } else { wait });
        }
    }

    pub fn hide_tip(&mut self) {
        self.kill_timer(TIMER_TIP);
        if self.tip.text.is_some() {
            self.tip.hide();
            self.tip_gone = Some(Instant::now());
        }
    }

    /// Shows the tooltip of what the mouse rests on: under it, or over it in the status bar.
    fn show_tip(&mut self) {
        self.kill_timer(TIMER_TIP);
        let (Some(text), Some(r)) = (self.tip_text(self.hover), self.hit_rect(self.hover)) else { return };
        let tl = client_to_screen(self.hwnd, self.dip_to_px(r.x), self.dip_to_px(r.y));
        let br = client_to_screen(self.hwnd, self.dip_to_px(r.right()), self.dip_to_px(r.bottom()));
        let r = windows::Win32::Foundation::RECT { left: tl.x, top: tl.y, right: br.x, bottom: br.y };
        self.tip.show(&text, r, matches!(self.hover, Hit::Status(_)));
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
            DragMode::Text { at, x: x0, y: y0, moved } => {
                // (it starts moving once the mouse has gone as far as Windows says a drag starts)
                let (dx, dy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                let (dx, dy) = (self.px_to_dip(dx).max(2.0), self.px_to_dip(dy).max(2.0));
                if !moved && (x - x0 as f32).abs() < dx && (y - y0 as f32).abs() < dy {
                    return;
                }
                self.tab_mut().view.drag = Some(DragMode::Text { at, x: x0, y: y0, moved: true });
                let r = self.r_edit;
                if y < r.y || y > r.bottom() {
                    self.timer(TIMER_SCROLL, 40);
                } else {
                    self.kill_timer(TIMER_SCROLL);
                }
                self.extend_drag(DragMode::Text { at, x: x0, y: y0, moved: true }, x, y);
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
                let geom = self.editor_geom();
                // (Without word wrap, past the left or right edge scrolls sideways.)
                let sideways = !self.style.wrap && (x < geom.text_x || x > geom.text_x + geom.text_w);
                if y < r.y || y > r.bottom() || sideways {
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
            DragMode::Text { moved: true, .. } => tab.view.drop_at = Some(pos),
            DragMode::Block => {
                let line = doc.line_of(pos).unwrap_or(0);
                let tab_size = self.settings.tab_size;
                let col = self.col_at_x(x).min(crate::edit::BLOCK_COLS_MAX);
                let tab = self.tab_mut();
                if let Some(b) = tab.view.block.as_mut() {
                    // (no more lines than a column selection takes)
                    let max = crate::edit::BLOCK_LINES_MAX - 1;
                    b.caret = (line.clamp(b.anchor.0.saturating_sub(max), b.anchor.0 + max), col);
                    tab.view.sel = Sel::at(crate::edit::pos_at_col(&tab.doc, b.caret.0, col, tab_size));
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    /// Keys for a column selection: Shift+Alt+arrows make or grow it, Backspace and Delete work on every line, Esc
    /// ends it. True when the key was used. (Typing, Copy, Cut and Paste go through `type_text` and the clipboard's
    /// functions; anything that moves the caret or edits otherwise ends it: `after_move`, `after_edit`.)
    fn block_key(&mut self, k: VIRTUAL_KEY, m: &Mods) -> bool {
        let tab_size = self.settings.tab_size;
        if m.alt && m.shift && !m.ctrl && matches!(k, VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT) {
            let tab = self.tab_mut();
            let caret = tab.view.sel.caret;
            let doc = &tab.doc;
            let mut b = match tab.view.block {
                Some(b) => b,
                None => match crate::edit::block_at(doc, caret, tab_size) {
                    Ok(b) => b,
                    Err(e) => {
                        self.flash(e, true);
                        return true;
                    }
                },
            };
            let last = doc.line_of(doc.len()).unwrap_or(0);
            let (line, col) = b.caret;
            b.caret = match k {
                VK_UP => (line.saturating_sub(1), col),
                VK_DOWN => ((line + 1).min(last), col),
                VK_LEFT => (line, col.saturating_sub(1)),
                _ => (line, (col + 1).min(crate::edit::BLOCK_COLS_MAX)),
            };
            if b.caret.0.abs_diff(b.anchor.0) >= crate::edit::BLOCK_LINES_MAX {
                return true;
            }
            let at = crate::edit::pos_at_col(doc, b.caret.0, b.caret.1, tab_size);
            tab.view.sel = Sel::at(at);
            self.after_move();
            self.tab_mut().view.block = Some(b);
            self.invalidate();
            return true;
        }
        let Some(b) = self.tab().view.block else { return false };
        match k {
            VK_BACK | VK_DELETE => {
                self.block_edit(b, crate::edit::BlockEdit::Delete { back: k == VK_BACK });
                true
            }
            VK_ESCAPE if !self.find.open => {
                self.tab_mut().view.block = None;
                self.invalidate();
                true
            }
            _ => false,
        }
    }

    /// `edit::block_edit` on the tab shown, the column selection after it kept.
    fn block_edit(&mut self, b: crate::edit::Block, what: crate::edit::BlockEdit) {
        if !self.editable() {
            return;
        }
        let tab_size = self.settings.tab_size;
        let tab = self.tab_mut();
        let after = crate::edit::block_edit(&mut tab.doc, &b, tab_size, what);
        tab.view.sel = Sel::at(crate::edit::pos_at_col(&tab.doc, after.caret.0, after.caret.1, tab_size));
        self.after_edit();
        self.tab_mut().view.block = Some(after);
        self.invalidate();
    }

    pub fn on_mouse_up(&mut self, x: f32, y: f32, button: u8) {
        let hit = self.hit(x, y);
        let down = match button {
            0 => std::mem::replace(&mut self.down, Hit::None),
            2 => std::mem::replace(&mut self.middle_down, Hit::None),
            _ => Hit::None,
        };
        unsafe {
            let _ = ReleaseCapture();
        }
        self.kill_timer(TIMER_SCROLL);
        if self.split_drag.take().is_some() {
            self.settings.save();
        }
        self.struct_drag = None;
        if !self.tabs.is_empty() {
            let drag = self.tab_mut().view.drag.take();
            let drop = self.tab_mut().view.drop_at.take();
            match (drag, drop) {
                // dropped: moved there (copied, with Ctrl)
                (Some(DragMode::Text { moved: true, .. }), Some(to)) => self.drop_text(to, mods().ctrl),
                // a click in the selection that didn't move: the caret goes there
                (Some(DragMode::Text { at, moved: false, .. }), _) => {
                    self.tab_mut().view.set_caret(at, false);
                    self.restart_caret();
                }
                _ => {}
            }
        }
        if button == 0 {
            self.close_find_after_click();
        }
        self.tab_drag = None;
        match (button, down, hit) {
            (0, Hit::TabClose(i), Hit::TabClose(j)) if i == j => self.pending.push(Deferred::Cmd(Cmd::CloseTabAt(i))),
            (0, Hit::NewTab, Hit::NewTab) => self.pending.push(Deferred::Cmd(Cmd::NewTab)),
            (0, Hit::TabList, Hit::TabList) => self.pending.push(Deferred::TabList),
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
                StatusItem::Update => self.pending.push(Deferred::UpdatePrompt),
                StatusItem::Overtype => self.pending.push(Deferred::Cmd(Cmd::ToggleOvertype)),
                StatusItem::ReadOnly => self.pending.push(Deferred::Cmd(Cmd::ToggleReadOnly)),
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
        self.hide_tip();
        if unsafe { GetCapture() } != self.hwnd {
            // (a press whose release can't come here any more)
            self.down = Hit::None;
            self.middle_down = Hit::None;
        }
        if self.hover != Hit::None {
            self.hover = Hit::None;
            self.invalidate();
        }
    }

    pub fn on_wheel(&mut self, delta: i32, horizontal: bool, x: f32, y: f32) {
        self.hide_tip();
        let m = mods();
        if m.ctrl && !horizontal {
            // One zoom step per notch (120); a touchpad's pinch sends many small steps, which add up.
            if (self.wheel_zoom > 0) != (delta > 0) {
                self.wheel_zoom = 0;
            }
            self.wheel_zoom += delta;
            while self.wheel_zoom.abs() >= 120 {
                let zoom_in = self.wheel_zoom > 0;
                self.wheel_zoom -= 120 * self.wheel_zoom.signum();
                self.exec(if zoom_in { Cmd::ZoomIn } else { Cmd::ZoomOut });
            }
            return;
        }
        if self.r_struct.w > 0.0 && self.r_struct.contains(x, y) {
            let s = &mut self.tab_mut().structure;
            s.scroll = (s.scroll - delta as f32 / 120.0 * 3.0 * super::structure::row_h()).max(0.0);
            self.invalidate();
            return;
        }
        if self.r_tabs.contains(x, y) {
            // A tab per notch (the strip scrolls by whole tabs; a touchpad's small steps add up).
            let tabs = -delta as f32 / 120.0;
            if (self.wheel_tabs > 0.0) != (tabs > 0.0) {
                self.wheel_tabs = 0.0;
            }
            self.wheel_tabs += tabs;
            let n = self.wheel_tabs.trunc();
            self.wheel_tabs -= n;
            let tw = self.tab_rects.first().map_or(0.0, |r| r.0.w);
            self.tab_scroll += n * tw;
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
        let per_notch = if lines == u32::MAX {
            // "one screen at a time"
            ((self.r_edit.h / self.style.row_h) as i64 - 1).max(1) as f32
        } else {
            lines.max(1) as f32
        };
        // Rows to scroll, fractions kept for the next message: a touchpad sends many small steps (and reversing
        // drops what's left over).
        let rows = -delta as f32 / 120.0 * per_notch;
        if (self.wheel_rows > 0.0) != (rows > 0.0) {
            self.wheel_rows = 0.0;
        }
        self.wheel_rows += rows;
        let n = self.wheel_rows.trunc();
        self.wheel_rows -= n;
        if n != 0.0 {
            self.with_view(|v, cx| v.scroll_rows(cx, n as i64));
            self.invalidate();
        }
    }

    pub fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_CARET => {
                // Like Windows' own caret, it stops blinking (shown) a while after the last key or click, so a window
                // left alone doesn't keep repainting for it.
                if CARET_SINCE.with(|c| c.get()).is_some_and(|t| t.elapsed() >= caret_timeout()) {
                    self.kill_timer(TIMER_CARET);
                    if !self.caret_on {
                        self.caret_on = true;
                        self.invalidate();
                    }
                    return;
                }
                self.caret_on = !self.caret_on;
                self.invalidate();
            }
            TIMER_FLASH | TIMER_COUNT | TIMER_STRUCTURE => {
                self.kill_timer(id);
                self.invalidate();
            }
            TIMER_TIP => self.show_tip(),
            TIMER_SPELL => {
                self.kill_timer(id);
                self.speller = super::spell::speller(win::testing()).map_or(Spelling::Unavailable, Spelling::Ready);
                self.invalidate();
                if std::mem::take(&mut self.f7_wanted) {
                    self.next_misspelling();
                }
            }
            TIMER_UNCLOAK => {
                self.kill_timer(id);
                if win::uncloak() {
                    self.invalidate();
                }
            }
            TIMER_REPAINT => {
                self.kill_timer(id);
                self.invalidate();
            }
            TIMER_FIND_CLOSE => self.close_find_now(),
            TIMER_F7 => {
                self.kill_timer(id);
                if self.f7_slice() {
                    self.timer(TIMER_F7, 1);
                }
            }
            TIMER_JOBS => self.poll_jobs(),
            TIMER_DISK => {
                self.check_disk();
                if self.session_dirty && self.settings.restore_session && self.last_session_save.elapsed() > Duration::from_secs(20) {
                    self.save_session_soon();
                }
            }
            TIMER_SEARCH => self.start_count(),
            TIMER_UPDATE => {
                // Again in an hour: Slate can stay open for days.
                self.timer(TIMER_UPDATE, 3_600_000);
                if self.settings.check_updates && unix_now().saturating_sub(self.settings.last_update_check) >= 20 * 3600 {
                    self.check_for_update(false);
                }
            }
            TIMER_SCROLL => {
                let Some(drag) = self.tabs.get(self.active).and_then(|t| t.view.drag) else {
                    self.kill_timer(TIMER_SCROLL);
                    return;
                };
                let (x, y) = match TEST_POINTER.with(|p| p.get()) {
                    Some(p) => p,
                    None => {
                        let mut pt = POINT::default();
                        unsafe {
                            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
                        }
                        (self.px_to_dip(pt.x), self.px_to_dip(pt.y))
                    }
                };
                let r = self.r_edit;
                let dist = if y < r.y { y - r.y } else if y > r.bottom() { y - r.bottom() } else { 0.0 };
                let geom = self.editor_geom();
                let (left, right) = (geom.text_x, geom.text_x + geom.text_w);
                let dx = if self.style.wrap {
                    0.0
                } else if x < left {
                    x - left
                } else if x > right {
                    x - right
                } else {
                    0.0
                };
                if dist != 0.0 || dx != 0.0 {
                    if dx != 0.0 {
                        // faster the further out the pointer is
                        let step = (dx.abs() / 2.0).clamp(4.0, 80.0) * dx.signum();
                        let v = &mut self.tab_mut().view;
                        let max = (v.content_w + 40.0 - geom.text_w).max(0.0);
                        v.scroll_x = (v.scroll_x + step).clamp(0.0, max);
                    }
                    let n = if dist != 0.0 { ((dist.abs() / 20.0).ceil() as i64).clamp(1, 20) * dist.signum() as i64 } else { 0 };
                    self.with_view(|v, cx| {
                        if n != 0 {
                            v.scroll_rows(cx, n);
                        }
                        // so the selection reaches what is under the pointer now, not before the scroll
                        v.layout_rows(cx);
                    });
                    let x = x.max(left).min((right - 1.0).max(left));
                    self.extend_drag(drag, x, y.max(r.y).min((r.bottom() - 1.0).max(r.y)));
                }
            }
            _ => {}
        }
    }

    /// Writes the session now (when that's on). Returns false if unsaved work couldn't be written to it.
    pub fn save_session(&mut self) -> bool {
        // One being written on another thread finishes first (the backups it writes are newer than those on disk).
        if let Some(mut job) = self.session_job.take() {
            if let Some(out) = job.wait() {
                session::finish(&mut self.tabs, out);
            }
        }
        self.last_session_save = Instant::now();
        self.session_dirty = false;
        !self.settings.restore_session || session::save(&mut self.tabs, self.active)
    }

    /// Writes the session on another thread (while editing: backups of big documents take a moment).
    pub fn save_session_soon(&mut self) {
        if self.session_job.is_some() {
            return;
        }
        self.last_session_save = Instant::now();
        self.session_dirty = false;
        self.session_job = session::start(&mut self.tabs, self.active, self.notify.clone());
        if self.session_job.is_some() {
            self.timer(TIMER_JOBS, 100);
        }
    }

    /// Picks up the session write when it's done; returns whether it's still going.
    fn poll_session(&mut self) -> bool {
        let Some(job) = self.session_job.as_mut() else { return false };
        let Some(out) = job.take() else { return true };
        self.session_job = None;
        if !session::finish(&mut self.tabs, out) {
            // Try again with the next round.
            self.session_dirty = true;
        }
        false
    }

    fn notice_action(&mut self, i: usize) {
        let Some(n) = self.tab().notice.as_ref() else { return };
        let Some((_, a)) = n.actions.get(i).cloned() else { return };
        match a {
            NoticeAction::Reload => {
                let i = self.active;
                if self.tabs[i].save.is_some() {
                    // (refused, said where it shows: see `Cmd::Reload`)
                    self.pending.push(Deferred::Cmd(Cmd::Reload));
                } else {
                    self.reload(i, None);
                }
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
            NoticeAction::Retry => {
                let i = self.active;
                self.start_restore(i);
            }
            NoticeAction::Recover => {
                let i = self.active;
                let notify = self.notify.clone();
                let Some(r) = self.tabs[i].restore.as_mut() else { return };
                if r.running() {
                    // (a look that's still going: when it ends, its answer counts)
                    self.flash("Still looking for the file — try again in a moment.", false);
                    return;
                }
                let Some(name) = r.st.pieces.clone() else { return };
                // (on another thread: the added text can be big)
                let list = r.big.clone();
                let total = list.as_ref().map_or(0, |l| l.header.data_len);
                let job = Job::spawn(total, notify, move |ctx| session::recover_big(&name, list.as_deref(), ctx));
                r.retry_at = None;
                r.job = Some(RestoreJob::Big(job));
                self.timer(TIMER_JOBS, 100);
            }
        }
        self.layout();
        self.invalidate();
    }

    // ---- commands without dialogs ----

    pub fn exec(&mut self, cmd: Cmd) {
        if self.tabs.is_empty() {
            self.new_untitled();
        }
        // (multiple cursors: what works with several keeps them; anything else leaves the main one)
        let keeps = matches!(
            cmd,
            Cmd::Copy
                | Cmd::Cut
                | Cmd::Paste
                | Cmd::ToggleMultiCursor
                | Cmd::AddCaretAbove
                | Cmd::AddCaretBelow
                | Cmd::AddNextMatch
                | Cmd::SkipMatch
                | Cmd::AllMatches
                | Cmd::LineStartCarets
                | Cmd::LineEndCarets
                | Cmd::RemoveLastCaret
                | Cmd::Find
                | Cmd::Replace
                | Cmd::ZoomIn
                | Cmd::ZoomOut
                | Cmd::ZoomReset
                | Cmd::ToggleWrap
                | Cmd::ToggleLineNumbers
                | Cmd::ToggleWhitespace
                | Cmd::CharInfo
        );
        if !keeps && !self.tab().view.carets.is_empty() {
            self.tab_mut().view.carets.clear();
            self.invalidate();
        }
        match cmd {
            Cmd::NewTab => self.new_untitled(),
            Cmd::Undo | Cmd::Redo => {
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let r = if cmd == Cmd::Undo { tab.doc.undo() } else { tab.doc.redo() };
                if let Some(mut sel) = r {
                    crate::edit::carets_after_undo(&tab.doc, &mut sel, &mut tab.view.carets, cmd == Cmd::Undo);
                    tab.view.sel = sel;
                    tab.view.upstream = false;
                    // (An edit out of view comes back into the middle of it, not at its edge.)
                    tab.sync_view();
                    self.reveal_caret(true);
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
                tab.view.block = None;
                tab.view.sel = Sel::new(0, tab.doc.len());
                self.invalidate();
            }
            Cmd::Find => self.open_find(BarMode::Find),
            Cmd::Replace => self.open_find(BarMode::Replace),
            Cmd::GoToLine => self.open_find(BarMode::GoTo),
            Cmd::GoToBracket | Cmd::SelectBrackets => {
                let tab = self.tab();
                let sel = tab.view.sel;
                let to = if cmd == Cmd::GoToBracket {
                    crate::edit::bracket_jump(&tab.doc, sel.caret).map(Sel::at)
                } else {
                    crate::edit::bracket_select(&tab.doc, sel.start(), sel.end()).map(|(a, b)| Sel::new(a, b))
                };
                match to {
                    Some(sel) => {
                        let tab = self.tab_mut();
                        tab.view.sel = sel;
                        tab.view.upstream = false;
                        tab.view.want_x = None;
                        self.after_move();
                    }
                    None => self.flash("No brackets around the caret.", false),
                }
            }
            Cmd::FindNext => self.find_next(true),
            Cmd::FindPrev => self.find_next(false),
            Cmd::DuplicateLine | Cmd::DeleteLine | Cmd::MoveLineUp | Cmd::MoveLineDown | Cmd::Indent | Cmd::Outdent => {
                if !self.editable() {
                    return;
                }
                let ind = self.indent_now();
                let ts = self.settings.tab_size;
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                tab.doc.seal();
                let r = match cmd {
                    Cmd::DuplicateLine => editor::duplicate(&mut tab.doc, sel),
                    Cmd::DeleteLine => editor::delete_lines(&mut tab.doc, sel).ok_or(editor::TOO_MANY_LINES),
                    Cmd::MoveLineUp => editor::move_lines(&mut tab.doc, sel, false),
                    Cmd::MoveLineDown => editor::move_lines(&mut tab.doc, sel, true),
                    Cmd::Indent => editor::indent_lines(&mut tab.doc, sel, ind, ts, false),
                    _ => editor::indent_lines(&mut tab.doc, sel, ind, ts, true),
                };
                tab.doc.seal();
                match r {
                    Ok(s) => {
                        tab.view.sel = s;
                        self.after_edit();
                    }
                    Err(why) => self.flash(why, true),
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
            Cmd::ToggleWhitespace => {
                self.settings.show_whitespace = !self.settings.show_whitespace;
                self.settings_changed();
            }
            Cmd::ToggleSpellCheck => {
                self.settings.spell_check = !self.settings.spell_check;
                self.settings_changed();
            }
            Cmd::ToggleTitleBarTabs => {
                self.settings.tabs_in_title_bar = !self.settings.tabs_in_title_bar;
                self.settings_changed();
                self.sync_frame();
            }
            Cmd::ToggleCloseWithLastTab => {
                self.settings.close_with_last_tab = !self.settings.close_with_last_tab;
                self.settings_changed();
            }
            Cmd::ToggleMultiCursor => {
                self.settings.multi_cursor = !self.settings.multi_cursor;
                if !self.settings.multi_cursor {
                    for t in &mut self.tabs {
                        t.view.carets.clear();
                    }
                }
                self.flash(
                    if self.settings.multi_cursor {
                        "Multiple cursors on: Ctrl+click adds or takes away one (Edit → Multiple cursors has the rest)"
                    } else {
                        "Multiple cursors off"
                    },
                    false,
                );
                self.settings_changed();
                self.invalidate();
            }
            Cmd::AddCaretAbove
            | Cmd::AddCaretBelow
            | Cmd::AddNextMatch
            | Cmd::SkipMatch
            | Cmd::AllMatches
            | Cmd::LineStartCarets
            | Cmd::LineEndCarets
            | Cmd::RemoveLastCaret => self.caret_cmd(cmd),
            Cmd::ToggleFindCloseOnLeave => {
                self.settings.find_close_on_leave = !self.settings.find_close_on_leave;
                self.settings_changed();
            }
            Cmd::ToggleFindPerTab => {
                self.settings.find_per_tab = !self.settings.find_per_tab;
                // (from now: the tabs' kept states were from the last time it was on)
                for t in &mut self.tabs {
                    t.find_bar = None;
                }
                self.find_owner = self.tab().id;
                self.settings_changed();
            }
            Cmd::OpenSelection => self.open_selection(None),
            Cmd::FindInTabs => self.list_in_tabs(),
            Cmd::ReplaceInTabs => self.replace_in_tabs(),
            Cmd::FindInFiles => self.find_in_files(),
            Cmd::ToggleBarsAtBottom => {
                self.settings.bars_at_bottom = !self.settings.bars_at_bottom;
                self.settings_changed();
                self.invalidate();
            }
            Cmd::UiSize(n) => {
                self.settings.ui_size = n;
                self.settings.save();
                self.apply_ui_size();
            }
            Cmd::ToggleHideMenuBar => {
                self.settings.hide_menu_bar = !self.settings.hide_menu_bar;
                self.settings.save();
                self.menu_bar_changed();
                if self.settings.hide_menu_bar {
                    self.flash("The menu bar shows when you press Alt (Settings → Hide the menu bar brings it back)", false);
                }
            }
            Cmd::GoBack => self.go_nav(true),
            Cmd::GoForward => self.go_nav(false),
            Cmd::ToggleReopenWhereLeft => {
                // (off: what was kept goes too)
                self.settings.reopen_where_left = !self.settings.reopen_where_left;
                if !self.settings.reopen_where_left {
                    self.settings.places.clear();
                }
                self.settings.save();
            }
            Cmd::OpenAt(p) => self.open_selection(Some(p)),
            Cmd::Suggestion(i) => self.spell_replace(i as usize),
            Cmd::SuggestionAll(i) => self.spell_replace_all(i as usize),
            Cmd::NextMisspelling => self.next_misspelling(),
            Cmd::AddWord | Cmd::IgnoreWord => {
                if let (Some(w), Spelling::Ready(s)) = (self.spell_word.take(), &mut self.speller) {
                    s.accept(&w.word, cmd == Cmd::AddWord);
                    self.invalidate();
                }
            }
            Cmd::ToggleReadOnly => {
                let tab = self.tab_mut();
                if tab.filter.is_some() || tab.results.is_some() {
                    self.flash("A tab of matching lines or results stays read only.", false);
                } else {
                    tab.read_only = !tab.read_only;
                    let on = tab.read_only;
                    self.session_dirty = true;
                    self.flash(if on { "Read only: the text can't be changed (File → Read only)" } else { "Editable" }, false);
                    self.layout();
                }
            }
            Cmd::FilterLines(invert) => self.filter_lines(invert),
            Cmd::ClearHighlights => {
                self.highlights.clear();
                self.invalidate();
            }
            Cmd::ToggleBookmark => {
                let tab = self.tab_mut();
                let caret = tab.view.sel.caret;
                crate::edit::toggle_mark(&mut tab.view.bookmarks, &tab.doc, caret);
                self.session_dirty = true;
            }
            Cmd::NextBookmark | Cmd::PrevBookmark => {
                let tab = self.tab();
                match crate::edit::next_mark(&tab.view.bookmarks, &tab.doc, tab.view.sel.caret, cmd == Cmd::NextBookmark) {
                    Some(p) => self.go_to(p, true),
                    None => self.flash("No bookmarks in this tab: Ctrl+F2 bookmarks the caret's line.", false),
                }
            }
            Cmd::ClearBookmarks => {
                self.tab_mut().view.bookmarks.clear();
                self.session_dirty = true;
            }
            Cmd::ToggleOvertype => {
                self.overtype = !self.overtype;
                self.restart_caret();
                let msg =
                    if self.overtype { "Overtype: typing replaces characters (Insert turns it off)" } else { "Overtype off" };
                self.flash(msg, false);
            }
            Cmd::ToggleRestoreSession => {
                self.settings.restore_session = !self.settings.restore_session;
                self.settings.save();
                self.session_dirty = true;
                let msg = if self.settings.restore_session {
                    "Slate will open with these tabs (and their unsaved changes) next time"
                } else {
                    "Slate will open with a new tab next time; closing asks about unsaved changes"
                };
                self.flash(msg, false);
            }
            Cmd::JsonIndent(n) => {
                self.settings.json_indent = n.clamp(1, 8);
                self.settings.save();
                let n = self.settings.json_indent as u64;
                self.flash(format!("Formatting indents by {}", plural(n, "space", "spaces")), false);
            }
            Cmd::ToggleStructure => {
                self.settings.structure_panel = !self.settings.structure_panel;
                if self.settings.structure_panel && !self.tab().lang.has_structure() {
                    self.flash("The structure panel shows JSON and XML files (Format → Language).", false);
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
                tab.structure.set_lang(tab.lang);
                let p = tab.structure.path_at_caret(&mut tab.doc, caret, &notify);
                let busy = tab.structure.busy();
                let what = if tab.structure.is_xml() { "XML" } else { "JSON" };
                match p {
                    Some(p) if !p.is_empty() => {
                        let s = tab.structure.path_text(&p);
                        if win::set_clipboard(self.hwnd, s.as_bytes()) {
                            self.flash(format!("Copied {s}"), false);
                        } else {
                            self.flash(CLIPBOARD_BUSY, true);
                        }
                    }
                    Some(_) => self.flash(format!("No {what} path here"), true),
                    None => self.flash(format!("Still reading the {what} structure — try again in a moment."), true),
                }
                if busy {
                    self.timer(TIMER_JOBS, 100);
                }
            }
            Cmd::CheckUpdates => self.check_for_update(true),
            Cmd::Update => self.pending.push(Deferred::UpdatePrompt),
            Cmd::ToggleAutoUpdate => {
                self.settings.check_updates = !self.settings.check_updates;
                self.settings.save();
                let on = self.settings.check_updates;
                self.flash(if on { "Slate looks for updates once a day" } else { "Slate won't look for updates by itself" }, false);
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
                let tab = self.tab();
                let f = match tab.lang {
                    Lang::Json => Fmt::Json,
                    Lang::Xml => Fmt::Xml,
                    Lang::Csv | Lang::CsvSemi | Lang::Tsv if cmd == Cmd::Validate => match crate::edit::csv_dialect(tab.lang, &tab.doc) {
                        Some(d) => Fmt::Csv(d),
                        None => return,
                    },
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
            Cmd::CharInfo => {
                let tab = self.tab();
                match crate::edit::char_info(&tab.doc, tab.view.sel.caret, tab.doc.encoding) {
                    Some(s) => self.flash(s, false),
                    None => self.flash("The caret is at the end of the text: no character there.", false),
                }
            }
            Cmd::SortByColumn(desc) => {
                let tab = self.tab();
                let Some(d) = crate::edit::csv_dialect(tab.lang, &tab.doc) else {
                    self.flash("Sort by this column works in CSV and TSV files (the language is in the status bar).", true);
                    return;
                };
                if !tab.doc.is_ready() {
                    self.flash("Still reading the file's lines — try again in a moment.", true);
                    return;
                }
                let Some((col, _)) = crate::edit::csv_field(&tab.doc, d, tab.view.sel.caret) else {
                    self.flash("Put the caret in the column to sort by (nearer its line's start).", true);
                    return;
                };
                let (_, header) = crate::edit::csv_header(&tab.doc, d);
                self.start_task(TaskKind::SortColumn { col, desc, d, header });
            }
            Cmd::ToggleComment => {
                let tab = self.tab();
                let Some(style) = crate::edit::comment_style(&tab.doc, tab.lang, tab.view.sel.start()) else {
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
                    Ok(s) => {
                        tab.view.sel = s;
                        self.after_edit();
                    }
                    Err(m) => self.flash(m, true),
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
                if count == 0 {
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
            Cmd::Convert(crate::convert::Conv::Sha256) => {
                let tab = self.tab();
                let sel = tab.view.sel;
                let (from, to) = if sel.is_empty() { (0, tab.doc.len()) } else { (sel.start(), sel.end()) };
                self.start_task(TaskKind::Hash { from, to });
            }
            Cmd::Convert(c) => {
                if !self.editable() {
                    return;
                }
                let tab = self.tab_mut();
                let sel = tab.view.sel;
                let (a, b) = if sel.is_empty() { (0, tab.doc.len()) } else { (sel.start(), sel.end()) };
                if b - a > crate::convert::CONVERT_MAX {
                    self.flash("Select up to 16 MB to convert.", true);
                    return;
                }
                if a == b {
                    self.flash("Nothing to convert: select some text (or have some in the tab).", false);
                    return;
                }
                let text = tab.doc.read(a, b);
                match c.apply(&text) {
                    Ok(out) => {
                        let n = out.len() as u64;
                        tab.view.sel = editor::replace_range(&mut tab.doc, sel, a, b, &out);
                        self.after_edit();
                        self.flash(c.done(n), false);
                    }
                    Err(why) => self.flash(why, true),
                }
            }
            Cmd::SetEol(e) => {
                // (also to the same: stray line endings of the other kind are converted)
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
                tab.lang_picked = true;
                tab.view.clear_cache();
                self.session_dirty = true;
                self.invalidate();
            }
            Cmd::LangForEnding => {
                let Some(k) = self.tab().lang_key() else { return };
                let lang = self.tab().lang;
                let (msg, on) = self.settings.toggle_lang_for(&k, lang);
                if on {
                    // (the other tabs of that ending or name follow, unless their language was picked by hand)
                    for t in self.tabs.iter_mut().filter(|t| !t.lang_picked && t.lang != lang && t.lang_key().as_ref() == Some(&k)) {
                        t.lang = lang;
                        t.view.clear_cache();
                    }
                }
                self.settings.save();
                self.flash(msg, false);
                self.invalidate();
            }
            Cmd::IndentSpaces(b) => {
                // For this document (picked: not guessed again), and the default for new ones.
                let ind = match (b, self.indent_now()) {
                    (false, _) => Indent::Tabs,
                    (true, Indent::Spaces(n)) => Indent::Spaces(n),
                    (true, Indent::Tabs) => Indent::Spaces(self.settings.tab_size),
                };
                let tab = self.tab_mut();
                tab.indent = Some(ind);
                tab.indent_picked = true;
                self.settings.use_spaces = b;
                self.settings_changed();
            }
            Cmd::TabSize(n) => {
                // How wide a tab shows, and a level of spaces (this document's too, when it's indented with them).
                if let Indent::Spaces(_) = self.indent_now() {
                    let tab = self.tab_mut();
                    tab.indent = Some(Indent::Spaces(n));
                    tab.indent_picked = true;
                }
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
            Cmd::ShowTab(id) => {
                if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                    self.activate(i);
                }
            }
            Cmd::ReopenClosed => {
                let Some(c) = self.closed_tabs.pop() else {
                    self.flash("No closed tabs to reopen", false);
                    return;
                };
                let before: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
                self.open_paths(std::slice::from_ref(&c.path));
                // A new tab (not one with that file open already): back where it was.
                let i = self.active;
                if !before.contains(&self.tabs[i].id) {
                    self.tabs[i].goto = Some(Goto::Place { caret: c.caret, top: c.top });
                    self.apply_goto(i);
                }
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
                    if win::set_clipboard(self.hwnd, p.to_string_lossy().as_bytes()) {
                        self.flash("Path copied", false);
                    } else {
                        self.flash(CLIPBOARD_BUSY, true);
                    }
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
                super::jumplist::update(Vec::new());
            }
            Cmd::ReportProblem => {
                // GitHub's issue form, filled in: the user sees it all, and sends it (or doesn't) themselves.
                win::shell_open(self.hwnd, std::ffi::OsStr::new(&super::crash::report_url()), "report a problem");
            }
            Cmd::OpenDataFolder => {
                let d = data_dir();
                let _ = std::fs::create_dir_all(&d);
                win::shell_open(self.hwnd, d.as_os_str(), "open the data folder");
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
        // (On character boundaries whatever happens: a selection ending inside a character would let typing split it.)
        let doc = &tab.doc;
        let on_char = |mut p: u64| {
            p = p.min(len);
            let from = p;
            while p > 0 && from - p < 3 && doc.byte_at(p).is_some_and(|b| b & 0xC0 == 0x80) {
                p -= 1;
            }
            p
        };
        let (start, end) = (on_char(start), on_char(end));
        tab.view.sel = if !container && end > start && end - start <= 64 * 1024 { Sel::new(start, end) } else { Sel::at(start) };
        tab.view.upstream = false;
        tab.view.want_x = None;
        tab.doc.seal();
        self.with_view(|v, cx| v.reveal(cx, start, true));
        self.restart_caret();
        let close = self.settings.find_close_on_leave && self.in_find_bar();
        win::set_focus(self.hwnd);
        if close {
            self.close_find();
        }
        self.invalidate();
    }

    /// Go to line with a JSON or XML path: its value.
    fn go_to_path(&mut self, text: &str) {
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        tab.structure.set_lang(tab.lang);
        match tab.structure.find_path(&mut tab.doc, text, &notify) {
            crate::structure::PathFound::At(a, b, container) => {
                self.close_find();
                self.jump_to_value(a, b, container);
            }
            crate::structure::PathFound::NotFound(why) => self.flash(why, true),
            crate::structure::PathFound::Reading => {
                self.flash("Still reading the structure — try again in a moment.", true);
                self.timer(TIMER_JOBS, 100);
            }
        }
    }

    fn jump_path(&mut self, i: usize) {
        if !self.tab().structure.path_current(&self.tab().doc) {
            self.flash("The path is being worked out again after your change — try again in a moment.", false);
            return;
        }
        let step = self.tab().structure.path.as_ref().and_then(|p| p.get(i)).cloned();
        if let Some(st) = step {
            let first = self.tab().doc.byte_at(st.start).unwrap_or(0);
            self.jump_to_value(st.start, st.end, matches!(first, b'{' | b'[' | b'<'));
        }
    }

    pub(super) fn struct_click(&mut self, i: usize, chevron: bool, double: bool) {
        let Some(row) = self.tab().structure.rows.get(i).cloned() else { return };
        if row.expandable && (chevron || double) {
            self.tab_mut().structure.toggle(row.key);
        }
        if !chevron && row.end > row.start {
            // (rows from before a change: their places in the text now)
            let Some((start, end)) = self.tab().structure.place_now(&self.tab().doc, row.start, row.end) else {
                self.flash("The structure is being updated after your change — try again in a moment.", false);
                self.invalidate();
                return;
            };
            self.tab_mut().structure.selected = Some(row.key);
            let container = row.kind.is_none_or(|k| k.is_container());
            self.jump_to_value(start, end, container);
        }
        self.invalidate();
    }

    // ---- menus ----

    pub fn menu_items(&mut self, idx: usize) -> Vec<Item> {
        let tab = self.tab();
        let has_path = tab.doc.path.is_some();
        let read_only = tab.read_only || tab.filter.is_some();
        let has_marks = !tab.view.bookmarks.is_empty();
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
                    item(Cmd::OpenSelection, "Open selected file or lin&k", ""),
                    sub("Open &recent", recent),
                    enabled(Cmd::ReopenClosed, "Reop&en closed tab", "Ctrl+Shift+T", !self.closed_tabs.is_empty()),
                    Item::Sep,
                    item(Cmd::Save, "&Save", "Ctrl+S"),
                    item(Cmd::SaveAs, "Save &as…", "Ctrl+Shift+S"),
                    item(Cmd::SaveAll, "Save a&ll", "Ctrl+Alt+S"),
                    Item::Sep,
                    item(Cmd::Print, "&Print…", "Ctrl+P"),
                    item(Cmd::PrintPreview, "Print previe&w…", ""),
                    item(Cmd::PageSetup, "Page set&up…", ""),
                    Item::Sep,
                    enabled(Cmd::Reload, "Reloa&d from disk", "", has_path),
                    enabled(Cmd::RevealFile, "Show in &folder", "", has_path),
                    enabled(Cmd::CopyPath, "Copy file pat&h", "", has_path),
                    check(Cmd::ToggleReadOnly, "Read onl&y", "", read_only),
                    Item::Sep,
                    item(Cmd::CloseTab, "&Close tab", "Ctrl+W"),
                    item(Cmd::CloseOthers, "Close o&ther tabs", ""),
                    item(Cmd::CloseSaved, "Close sa&ved tabs", ""),
                    item(Cmd::CloseAll, "Close all ta&bs", ""),
                    Item::Sep,
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
                item(Cmd::FindNext, "Find ne&xt", "F3"),
                item(Cmd::FindPrev, "Find pre&vious", "Shift+F3"),
                item(Cmd::Replace, "R&eplace…", "Ctrl+H"),
                item(Cmd::GoToLine, "&Go to line…", "Ctrl+G"),
                // (in groups, as on Linux; no letters: every one in their words is another item's)
                Item::Sep,
                item(Cmd::FindInTabs, "Find in all tabs", ""),
                item(Cmd::ReplaceInTabs, "Replace in all tabs", ""),
                item(Cmd::FindInFiles, "Find in files", "Ctrl+Shift+F"),
                Item::Sep,
                item(Cmd::GoBack, "Go back", "Alt+Left"),
                item(Cmd::GoForward, "Go forward", "Alt+Right"),
                item(Cmd::GoToBracket, "Go to matching &bracket", "Ctrl+M"),
                item(Cmd::SelectBrackets, "Select inside brackets", "Ctrl+Shift+M"),
                sub(
                    "Boo&kmarks",
                    vec![
                        item(Cmd::ToggleBookmark, "&Toggle bookmark", "Ctrl+F2"),
                        item(Cmd::NextBookmark, "&Next bookmark", "F2"),
                        item(Cmd::PrevBookmark, "&Previous bookmark", "Shift+F2"),
                        Item::Sep,
                        enabled(Cmd::ClearBookmarks, "&Clear bookmarks", "", has_marks),
                    ],
                ),
                Item::Sep,
                item(Cmd::SelectAll, "Select &all", "Ctrl+A"),
                // (no letter: every one in it is another item's)
                sub("Multiple cursors", {
                    let on = self.settings.multi_cursor;
                    vec![
                        check(Cmd::ToggleMultiCursor, "&Use multiple cursors (Ctrl+click)", "", on),
                        Item::Sep,
                        item(Cmd::AddCaretAbove, "Add cursor &above", "Ctrl+Shift+Up"),
                        item(Cmd::AddCaretBelow, "Add cursor &below", "Ctrl+Shift+Down"),
                        item(Cmd::AddNextMatch, "Add &next match", "Ctrl+Shift+N"),
                        item(Cmd::SkipMatch, "S&kip to next match", ""),
                        item(Cmd::AllMatches, "Select all &matches", "Ctrl+Shift+A"),
                        item(Cmd::LineStartCarets, "Cursors at line &starts", "Alt+["),
                        item(Cmd::LineEndCarets, "Cursors at line &ends", "Alt+]"),
                        item(Cmd::RemoveLastCaret, "&Remove last cursor", ""),
                    ]
                }),
                Item::Sep,
                item(Cmd::DuplicateLine, "Dupl&icate line", "Ctrl+D"),
                item(Cmd::DeleteLine, "Delete li&ne", "Ctrl+Shift+K"),
                item(Cmd::MoveLineUp, "M&ove line up", "Alt+Up"),
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
                // (what the text shows; how Slate looks and works is under Settings: issue #14)
                let s = &self.settings;
                let lang = self.tab().lang;
                let mut v = vec![
                    check(Cmd::ToggleWrap, "&Word wrap", "Alt+Z", s.wrap),
                    check(Cmd::ToggleWhitespace, "Show w&hitespace", "", s.show_whitespace),
                    check(Cmd::ToggleSpellCheck, if matches!(self.speller, Spelling::Unavailable) { "Spell chec&k (Windows has no dictionary for it)" } else { "Spell chec&k" }, "", s.spell_check),
                    item(Cmd::NextMisspelling, "Ne&xt word spelled wrong", "F7"),
                    sub(
                        "&Zoom",
                        vec![
                            item(Cmd::ZoomIn, "Zoom &in", "Ctrl+Plus"),
                            item(Cmd::ZoomOut, "Zoom &out", "Ctrl+Minus"),
                            item(Cmd::ZoomReset, "&Reset zoom", "Ctrl+0"),
                        ],
                    ),
                    Item::Sep,
                    sub(
                        "Matching &lines",
                        vec![
                            item(Cmd::FilterLines(false), "Show only &matching lines", "Alt+L"),
                            item(Cmd::FilterLines(true), "Show lines &not matching", ""),
                            enabled(Cmd::ClearHighlights, "Clear hi&ghlights", "", !self.highlights.is_empty()),
                        ],
                    ),
                    item(Cmd::CharInfo, "&Character at the caret", ""),
                ];
                if lang.has_structure() {
                    let what = if lang == Lang::Xml { "XML" } else { "JSON" };
                    v.push(Item::Sep);
                    v.push(check(Cmd::ToggleStructure, &format!("{what} &structure panel"), "Ctrl+Shift+O", s.structure_panel));
                    v.push(check(Cmd::TogglePathBar, &format!("{what} &path bar"), "", s.path_bar));
                }
                v
            }
            4 => {
                // Settings: how Slate looks and works (issue #14)
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
                vec![
                    check(Cmd::ToggleLineNumbers, "&Line numbers", "", s.line_numbers),
                    check(Cmd::ToggleTitleBarTabs, "&Tabs in the title bar", "", s.tabs_in_title_bar && !self.theme.hc),
                    check(Cmd::ToggleHideMenuBar, "Hide the men&u bar", "", s.hide_menu_bar),
                    sub(
                        "Interface te&xt size",
                        [0u32, 100, 125, 150, 175, 200]
                            .iter()
                            .map(|&n| {
                                // (each its own letter: 1, 2, 5, 7, 0)
                                let label = match n {
                                    0 => "As &Windows has it (Text size)".to_string(),
                                    100 => "&100%".to_string(),
                                    200 => "2&00%".to_string(),
                                    _ => format!("1&{}%", n - 100),
                                };
                                let on = if n == 0 { s.ui_size < 100 } else { s.ui_size == n };
                                check(Cmd::UiSize(n), &label, "", on)
                            })
                            .collect(),
                    ),
                    sub(
                        "Fin&d bar",
                        vec![
                            check(Cmd::ToggleFindCloseOnLeave, "&Close when you click in the text", "", s.find_close_on_leave),
                            check(Cmd::ToggleFindPerTab, "&One for each tab", "", s.find_per_tab),
                            check(Cmd::ToggleBarsAtBottom, "At the &bottom (notices too)", "", s.bars_at_bottom),
                        ],
                    ),
                    sub("&Font", font_items),
                    sub(
                        "Th&eme",
                        vec![
                            check(Cmd::Theme(ThemeMode::System), "Use &system setting", "", s.theme == ThemeMode::System),
                            check(Cmd::Theme(ThemeMode::Light), "&Light", "", s.theme == ThemeMode::Light),
                            check(Cmd::Theme(ThemeMode::Dark), "&Dark", "", s.theme == ThemeMode::Dark),
                        ],
                    ),
                    Item::Sep,
                    check(Cmd::ToggleCloseWithLastTab, "Close Slate with the last ta&b", "", s.close_with_last_tab),
                    check(Cmd::ToggleRestoreSession, "Restore last sess&ion", "", s.restore_session),
                    check(Cmd::ToggleReopenWhereLeft, "Reopen files &where you left them", "", s.reopen_where_left),
                    check(Cmd::ToggleAutoUpdate, "Check for updates aut&omatically", "", s.check_updates),
                    Item::Sep,
                    item(Cmd::ChangeShortcuts, "&Change keyboard shortcuts…", ""),
                    if super::install::associated() {
                        item(Cmd::StopDefault, "Stop opening files with Sl&ate…", "")
                    } else {
                        item(Cmd::MakeDefault, "Open files with Sl&ate…", "")
                    },
                    item(Cmd::OpenDataFolder, "Open settings folde&r", ""),
                ]
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
                if matches!(tab.lang, Lang::Csv | Lang::CsvSemi | Lang::Tsv) {
                    let what = if tab.lang == Lang::Tsv { "TSV" } else { "CSV" };
                    v.push(item(Cmd::Validate, &format!("Chec&k {what}"), ""));
                    v.push(item(Cmd::SortByColumn(false), "Sort by this column, &A to Z", ""));
                    v.push(item(Cmd::SortByColumn(true), "Sort by this column, &Z to A", ""));
                    v.push(Item::Sep);
                }
                if let Some(f) = f {
                    v.push(item(Cmd::Format, &format!("&Format {f}"), "Shift+Alt+F"));
                    v.push(item(Cmd::Minify, &format!("&Minify {f}"), ""));
                    v.push(item(Cmd::Validate, &format!("Chec&k {f}"), ""));
                    let widths =
                        [2u32, 3, 4, 8].iter().map(|&n| check(Cmd::JsonIndent(n), &format!("&{n} spaces"), "", s.json_indent == n));
                    v.push(sub("Formatting in&dent", widths.collect()));
                    v.push(Item::Sep);
                }
                v.push(sub("Con&vert", self.convert_items()));
                v.push(Item::Sep);
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
                    sub("&Indentation", self.indent_items()),
                ]);
                v
            }
            _ => {
                let update = match &self.update {
                    UpdateState::Available(r) => item(Cmd::Update, &format!("&Update to {}…", r.version), ""),
                    UpdateState::Checking { .. } => enabled(Cmd::CheckUpdates, "Checking for updates…", "", false),
                    UpdateState::Downloading { .. } => enabled(Cmd::CheckUpdates, "Downloading the update…", "", false),
                    UpdateState::Idle => item(Cmd::CheckUpdates, "Check for &updates…", ""),
                };
                vec![
                    item(Cmd::Shortcuts, "&Keyboard shortcuts", ""),
                    item(Cmd::ReportProblem, "&Report a problem…", ""),
                    Item::Sep,
                    update,
                    Item::Sep,
                    item(Cmd::About, "&About Slate", ""),
                ]
            }
        }
    }

    /// Format → Convert: what's done to the selection (or the whole text when nothing is selected).
    fn convert_items(&self) -> Vec<Item> {
        use crate::convert::Conv;
        let mut v = Vec::new();
        for c in Conv::ALL {
            if matches!(c, Conv::UrlEncode | Conv::JsonEscape | Conv::Sha256) {
                v.push(Item::Sep);
            }
            v.push(item(Cmd::Convert(c), c.label(), ""));
        }
        v
    }

    /// Format → Indentation, also the status bar's indentation menu.
    pub fn indent_items(&self) -> Vec<Item> {
        let ind = self.indent_now();
        let width = match ind {
            Indent::Spaces(n) => n,
            Indent::Tabs => self.settings.tab_size,
        };
        vec![
            check(Cmd::IndentSpaces(true), "&Spaces", "", ind != Indent::Tabs),
            check(Cmd::IndentSpaces(false), "&Tabs", "", ind == Indent::Tabs),
            Item::Sep,
            check(Cmd::TabSize(2), "Width &2", "", width == 2),
            check(Cmd::TabSize(4), "Width &4", "", width == 4),
            check(Cmd::TabSize(8), "Width &8", "", width == 8),
        ]
    }

    /// The list of all tabs (the tab strip's button when they don't all fit): choosing one shows it.
    pub fn tab_list_items(&self) -> Vec<Item> {
        (0..self.tabs.len())
            .map(|i| {
                let t = &self.tabs[i];
                let dirty = if t.doc.is_dirty() { "  \u{25CF}" } else { "" };
                check(Cmd::ShowTab(t.id), &format!("{}{dirty}", self.tab_label(i).replace('&', "&&")), "", i == self.active)
            })
            .collect()
    }

    /// Format → Language, also the status bar's language menu: plain text, then the languages in groups by their first
    /// letter (`Lang::menu_groups`). Not in columns: Windows draws a menu with columns in light colors even when menus
    /// are dark (issue #16).
    pub fn lang_items(&self) -> Vec<Item> {
        let cur = self.tab().lang;
        let mut v = vec![check(Cmd::SetLang(Lang::Plain), Lang::Plain.label(), "", cur == Lang::Plain), Item::Sep];
        for (letters, langs) in Lang::menu_groups() {
            let items = langs.iter().map(|&l| check(Cmd::SetLang(l), l.label(), "", l == cur)).collect();
            // (the group with the language in use says so)
            let label = match langs.iter().find(|&&l| l == cur) {
                Some(l) => format!("&{letters}\t{}", l.label()),
                None => format!("&{letters}"),
            };
            v.push(sub(&label, items));
        }
        // (this file's ending or name, always in this language: unless that's what Slate does anyway)
        let path = self.tab().doc.path.as_ref().map(|p| p.to_string_lossy().into_owned());
        if let (Some(k), Some(path)) = (self.tab().lang_key(), path) {
            let set = self.settings.lang_for(&k);
            if set.is_some() || !crate::highlight::own_choice(&path, cur) {
                v.push(Item::Sep);
                let label = k.item(cur).replace('&', "&&").replacen("Always", "Al&ways", 1);
                v.push(check(Cmd::LangForEnding, &label, "", set == Some(cur)));
            }
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

thread_local! {
    /// When the caret last moved, or a key or the IME was used in the text: it stops blinking a while after.
    static CARET_SINCE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// The caret moved, or a key or the IME was used in the text, now: it blinks for a while again (also where the App
/// is borrowed: a focus coming back while it is).
pub fn caret_moved() {
    CARET_SINCE.with(|c| c.set(Some(Instant::now())));
}

/// Windows' *Underline access keys* setting (SPI_GETKEYBOARDCUES): menu letters underlined all the time.
pub fn keyboard_cues() -> bool {
    let mut on = windows::Win32::Foundation::BOOL(0);
    let got = unsafe {
        SystemParametersInfoW(
            windows::Win32::UI::WindowsAndMessaging::SPI_GETKEYBOARDCUES,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    got.is_ok() && on.as_bool()
}

/// How long the caret blinks after it last moved: Windows' setting (SPI_GETCARETTIMEOUT), 5 s unless changed.
fn caret_timeout() -> Duration {
    let mut ms = 0u32;
    let got = unsafe {
        SystemParametersInfoW(
            SPI_GETCARETTIMEOUT,
            0,
            Some(&mut ms as *mut u32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    Duration::from_millis(if got.is_ok() && ms > 0 { ms as u64 } else { 5000 })
}

/// Files where Tab must insert a real tab: tab-separated data, and makefiles (a recipe line starts with one).
fn needs_tabs(t: &Tab) -> bool {
    if t.lang == Lang::Tsv {
        return true;
    }
    let name = t.doc.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_ascii_lowercase());
    name.is_some_and(|n| matches!(n.as_str(), "makefile" | "gnumakefile" | "bsdmakefile") || n.ends_with(".mk") || n.ends_with(".mak"))
}

/// Where line `line` (1-based, at most the last) and column `col` (in characters, 1-based, at most the line's end)
/// are; None while the document's lines are still being counted.
fn line_col_pos(doc: &Document, line: u64, col: Option<u64>) -> Option<u64> {
    let count = doc.line_count()?;
    let line = line.max(1).min(count.max(1));
    let ls = doc.line_start(line - 1).unwrap_or(0);
    Some(match col {
        Some(c) if c > 1 => skip_chars(doc, ls, doc.line_end_of(ls), c - 1),
        _ => ls,
    })
}

/// The place `n` characters after `from` (a character start), at most `end`. Characters are counted as the status
/// bar's column counts them (each byte that isn't a UTF-8 continuation byte starts one), a piece of text at a time,
/// so a column far into an 800 MB line is still quick.
fn skip_chars(doc: &Document, from: u64, end: u64, n: u64) -> u64 {
    let (mut left, mut pos, mut found) = (n, from, None);
    doc.chunks(from, end, &mut |c| {
        let here = bytecount::num_chars(c) as u64;
        if here <= left {
            left -= here;
            pos += c.len() as u64;
            return true;
        }
        // the start of character `left` (counting from 0) in this piece
        let i = c.iter().enumerate().filter(|(_, b)| !is_continuation(**b)).nth(left as usize).map_or(c.len(), |(i, _)| i);
        found = Some(pos + i as u64);
        false
    });
    found.unwrap_or(end)
}

/// A search as a tab's title shows it: 30 characters at most.
fn short_query(q: &str) -> String {
    if q.chars().count() > 30 { q.chars().take(29).collect::<String>() + "…" } else { q.to_string() }
}

/// The name of the folder file `p` is in.
fn root_name(p: Option<&Path>) -> String {
    p.and_then(|p| p.parent()).and_then(|d| d.file_name()).map_or_else(|| "its folder".into(), |n| n.to_string_lossy().into_owned())
}

/// A file name ending in `:120` or `:120:5` (a line, and a column), as editors take on their command line: the file
/// and where to go in it. Only the end of the name counts, so a drive's colon never does.
fn line_suffix(p: &Path) -> Option<(PathBuf, u64, Option<u64>)> {
    let name = p.file_name()?.to_str()?;
    let number = |s: &str| {
        let digits = !s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit());
        if digits { s.parse::<u64>().ok() } else { None }
    };
    let (rest, last) = name.rsplit_once(':')?;
    let last = number(last)?;
    let (base, line, col) = match rest.rsplit_once(':').and_then(|(b, l)| Some((b, number(l)?))) {
        Some((base, line)) => (base, line, Some(last)),
        None => (rest, last, None),
    };
    if base.is_empty() {
        return None;
    }
    Some((p.with_file_name(base), line, col))
}

/// Tells the Slate waiting with `--wait` (its event: only one of Slate's names) that its tabs are closed.
pub fn signal_wait(name: &str) {
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    if !name.starts_with("Local\\Slate.Wait.") {
        return;
    }
    unsafe {
        if let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(name)) {
            let _ = SetEvent(h);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------------------------------------------
// Things that may open modal UI: run outside the App borrow.

thread_local! {
    static MENU_SWITCH: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static MENU_BAR: std::cell::RefCell<(Vec<windows::Win32::Foundation::RECT>, usize, bool, bool)> =
        const { std::cell::RefCell::new((Vec::new(), 0, false, false)) };
}

/// Message filter during menu tracking: Left/Right and hovering move between the menu bar's menus.
// (a panic can't unwind out of a call from Windows: caught, logged by the panic hook, and the message passed on)
unsafe extern "system" fn menu_hook(code: i32, wp: windows::Win32::Foundation::WPARAM, lp: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HHOOK};
    std::panic::catch_unwind(|| unsafe { menu_message(code, wp, lp) })
        .unwrap_or_else(|_| unsafe { CallNextHookEx(HHOOK::default(), code, wp, lp) })
}

unsafe fn menu_message(code: i32, wp: windows::Win32::Foundation::WPARAM, lp: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
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

/// Shows a popup menu at client DIPs (x, y), below that point or (`up`) above it; returns the chosen command.
/// `name` says which menu it is (test mode notes it instead of showing it).
fn popup(cell: &Cell, name: &str, items: Vec<Item>, x: f32, y: f32, up: bool) -> Option<Cmd> {
    let items = super::commands::keyed(items, &cell.borrow().keymap);
    if let Some(chosen) = win::scripted_menu(name, &items) {
        return chosen;
    }
    let (hwnd, px, py) = {
        let a = cell.borrow();
        (a.hwnd, a.dip_to_px(x), a.dip_to_px(y))
    };
    let mut ids = Vec::new();
    let menu = build_menu(&items, &mut ids);
    let p = client_to_screen(hwnd, px, py);
    TOP_MENU.with(|t| t.set(menu.0 as isize));
    let align = if up { TPM_BOTTOMALIGN } else { TPM_TOPALIGN };
    let id = unsafe { TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_LEFTALIGN | align).0, p.x, p.y, hwnd, None) };
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
            // (a hidden menu bar shows while its menu is open: Settings → Hide the menu bar)
            a.menu_bar_changed();
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
        let chosen = popup(cell, MENU_TITLES[idx], items, rect.x, rect.bottom(), false);
        if let Some(h) = hook {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
        MENU_BAR.with(|b| b.borrow_mut().0.clear());
        {
            let mut a = cell.borrow_mut();
            a.menu_open = None;
            a.menu_bar_changed();
        }
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
            // (drawn first, while nothing has the App: F7's word shows selected under its menu. Asking the spell
            // checker for suggestions below can let a paint through while the App is busy, which is then skipped.)
            let hwnd = cell.borrow().hwnd;
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::UpdateWindow(hwnd);
            }
            let spelling = cell.borrow_mut().spelling_at(x, y);
            // (a file name or a link where it opens, or at the caret for the keyboard's menu: open it, issue #7)
            let open = {
                let mut a = cell.borrow_mut();
                let at = if std::mem::take(&mut a.menu_at_caret) { a.tab().view.sel.caret } else { a.with_view(|v, cx| v.pos_at(cx, x, y)).0 };
                let tab = a.tab();
                crate::links::target_in(&tab.doc, tab.view.sel, Some(at), true).ok().map(|t| (crate::links::menu_label(&t, tab.doc.path.as_deref().and_then(Path::parent), true), at))
            };
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
                if matches!(tab.lang, Lang::Csv | Lang::CsvSemi | Lang::Tsv) {
                    let what = if tab.lang == Lang::Tsv { "TSV" } else { "CSV" };
                    v.push(Item::Sep);
                    v.push(item(Cmd::Validate, &format!("Chec&k {what}"), ""));
                    v.push(item(Cmd::SortByColumn(false), "Sort by this col&umn, A to Z", ""));
                    v.push(item(Cmd::SortByColumn(true), "Sort by this colum&n, Z to A", ""));
                }
                if tab.lang == Lang::Json {
                    v.push(item(Cmd::CopyJsonPath, "Copy JSON pat&h", ""));
                } else if tab.lang == Lang::Xml {
                    v.push(item(Cmd::CopyJsonPath, "Copy XML pat&h (XPath)", ""));
                }
                if let Some((label, at)) = open {
                    v.splice(0..0, [item(Cmd::OpenAt(at), &label, ""), Item::Sep]);
                }
                // a word spelled wrong: what it may have been meant to be, first
                if let Some(sugg) = spelling {
                    let mut first: Vec<Item> = sugg.iter().enumerate().map(|(i, s)| item(Cmd::Suggestion(i as u8), &s.replace('&', "&&"), "")).collect();
                    if first.is_empty() {
                        first.push(enabled(Cmd::Suggestion(0), "(No suggestions)", "", false));
                    }
                    // (every match of the word: issue #12)
                    if !sugg.is_empty() {
                        let word = a.spell_word.as_ref().map(|w| w.word.replace('&', "&&")).unwrap_or_default();
                        let all = sugg.iter().enumerate().map(|(i, s)| item(Cmd::SuggestionAll(i as u8), &s.replace('&', "&&"), "")).collect();
                        first.push(sub(&format!("Change &every “{word}” to"), all));
                    }
                    first.extend([Item::Sep, item(Cmd::AddWord, "&Add to dictionary", ""), item(Cmd::IgnoreWord, "&Ignore", ""), Item::Sep]);
                    v.splice(0..0, first);
                }
                v
            };
            if let Some(c) = popup(cell, "context", items, x, y, false) {
                run_cmd(cell, c);
            }
        }
        Deferred::TabMenu(i, x, y) => {
            let Some(id) = cell.borrow().tabs.get(i).map(|t| t.id) else { return };
            let has_path = cell.borrow().tabs[i].doc.path.is_some();
            let reopen = !cell.borrow().closed_tabs.is_empty();
            let items = vec![
                item(Cmd::CloseTab, "&Close", "Ctrl+W"),
                item(Cmd::CloseOthers, "Close &others", ""),
                item(Cmd::CloseRight, "Close tabs to the &right", ""),
                item(Cmd::CloseSaved, "Close sa&ved", ""),
                item(Cmd::CloseAll, "Close &all", ""),
                Item::Sep,
                enabled(Cmd::ReopenClosed, "Reop&en closed tab", "Ctrl+Shift+T", reopen),
                Item::Sep,
                enabled(Cmd::CopyPath, "Copy &path", "", has_path),
                enabled(Cmd::RevealFile, "Show in &folder", "", has_path),
            ];
            cell.borrow_mut().activate(i);
            if let Some(c) = popup(cell, "tab", items, x, y, false) {
                // Other things can happen while the menu is open: only act if that tab is still the active one.
                if cell.borrow().tab().id == id {
                    run_cmd(cell, c);
                }
            }
        }
        Deferred::AskLossy(id) => ask_lossy(cell, id),
        Deferred::Hash => {
            let (hwnd, hash) = {
                let mut a = cell.borrow_mut();
                (a.hwnd, a.hash.take())
            };
            let Some((what, hex)) = hash else { return };
            let q = format!("SHA-256 of {what}");
            if win::ask(hwnd, "Slate", &q, &hex, &["&Copy", "OK"]) == Some(0) {
                let copied = win::set_clipboard(hwnd, hex.as_bytes());
                cell.borrow_mut().flash(if copied { "SHA-256 copied" } else { CLIPBOARD_BUSY }, !copied);
            }
        }
        Deferred::TabList => {
            let (items, r) = {
                let a = cell.borrow();
                (a.tab_list_items(), a.tablist_rect)
            };
            if let Some(c) = popup(cell, "tabs", items, r.x, r.bottom(), false) {
                run_cmd(cell, c);
            }
        }
        Deferred::Frame => {
            use windows::Win32::UI::WindowsAndMessaging::{SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
            let hwnd = cell.borrow().hwnd;
            unsafe {
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            let mut a = cell.borrow_mut();
            a.layout();
            a.invalidate();
        }
        Deferred::UpdatePrompt => {
            let (rel, hwnd) = {
                let a = cell.borrow();
                match &a.update {
                    UpdateState::Available(r) => (r.clone(), a.hwnd),
                    _ => return,
                }
            };
            let q = format!("Slate {} is available", rel.version);
            let detail = format!(
                "You have {}. Slate downloads the new version from GitHub and restarts; your tabs and unsaved changes come back.",
                Version::current()
            );
            match win::ask(hwnd, "Slate", &q, &detail, &["&Update and restart", "&What's new", "&Not now"]) {
                Some(0) => cell.borrow_mut().start_update(rel),
                Some(1) => update::show_page(&rel),
                _ => {}
            }
        }
        Deferred::StatusMenu(item_kind) => {
            let (items, rect) = {
                let a = cell.borrow();
                let rect = a.status_rects.iter().find(|(k, _)| *k == item_kind).map(|(_, r)| *r).unwrap_or_default();
                let items = match item_kind {
                    StatusItem::Lang => a.lang_items(),
                    StatusItem::Encoding => {
                        // (what the caret is on, first: the encoding's bytes for it)
                        let t = a.tab();
                        let info = crate::edit::char_info(&t.doc, t.view.sel.caret, t.doc.encoding);
                        let mut v: Vec<Item> = info.map(|i| vec![enabled(Cmd::CharInfo, &i.replace('&', "&&"), "", false), Item::Sep]).unwrap_or_default();
                        v.extend(a.encoding_items());
                        v
                    }
                    StatusItem::Indent => a.indent_items(),
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
            if let Some(c) = popup(cell, "status bar", items, rect.x, rect.y, true) {
                run_cmd(cell, c);
            }
        }
    }
}

/// How the tab's text up to `to` looks on paper, from the settings.
pub fn print_style(s: &crate::settings::Settings, tab: &Tab, to: u64) -> super::print::Style {
    super::print::Style {
        font: s.font.clone(),
        size: s.font_size,
        numbers: s.print_line_numbers,
        tab: s.tab_size as usize,
        title: tab.title(),
        last_line: tab.doc.line_of(to).unwrap_or(0) + 1,
        theme: crate::theme::Theme::light(crate::theme::rgb(0x0067C0)),
        colors: s.print_colors,
        header: s.print_header,
        margins: s.print_margins,
    }
}

/// File → Page setup: Windows' dialog for the printer chosen last (paper, orientation, margins); the margins are the
/// settings from then on.
fn page_setup(cell: &Cell) {
    // (test mode never shows it)
    if super::win::SCRIPTED.with(|s| s.borrow_mut().as_mut().map(|s| s.asked.push("Page setup".into())).is_some()) {
        return;
    }
    let (hwnd, mut last, mut margins) = {
        let mut a = cell.borrow_mut();
        (a.hwnd, std::mem::take(&mut a.print_last), a.settings.print_margins)
    };
    let ok = last.page_setup(hwnd, &mut margins);
    let mut a = cell.borrow_mut();
    a.print_last = last;
    if ok {
        a.settings.print_margins = margins;
        a.settings.save();
    }
}

/// File → Print preview: the pages as they'll print, in a window of their own (`preview::run`), whose Print goes on
/// to the print dialog (`print_tab`). The options and margins changed there are the settings from then on.
fn preview_tab(cell: &Cell) {
    let (hwnd, busy, counted) = {
        let a = cell.borrow();
        let t = a.tab();
        (a.hwnd, t.task.is_some() || t.save.is_some(), t.doc.line_count().is_some())
    };
    if busy {
        cell.borrow_mut().flash("Still working on the previous task (Esc cancels it).", true);
        return;
    }
    if !counted {
        cell.borrow_mut().flash("Print preview works once the file's lines are counted (a moment).", true);
        return;
    }
    // (test mode never shows it: `previewshot:` draws it into a picture, `previewrun:` runs it hidden)
    let hidden = super::preview::SCRIPT.with(|s| s.borrow().is_some());
    if super::win::SCRIPTED.with(|s| s.borrow_mut().as_mut().map(|s| s.asked.push("Print preview".into())).is_some()) && !hidden {
        return;
    }
    let (title, source, style, theme, notify, last) = {
        let mut a = cell.borrow_mut();
        let i = a.active;
        let snap = a.tabs[i].doc.snapshot();
        let tab = &a.tabs[i];
        let to = snap.len();
        let source = super::preview::Source { snap, from: 0, to, line: 1, line_start: 0, lang: tab.lang };
        let style = print_style(&a.settings, tab, to);
        let (title, theme, notify) = (tab.title(), a.theme.clone(), a.notify.clone());
        let last = std::mem::take(&mut a.print_last);
        (title, source, style, theme, notify, last)
    };
    let (outcome, last, changed) = super::preview::run(hwnd, &title, source, style, last, theme, notify);
    {
        let mut a = cell.borrow_mut();
        a.print_last = last;
        if let Some(st) = changed {
            let s = &mut a.settings;
            (s.print_colors, s.print_line_numbers, s.print_header, s.print_margins) = (st.colors, st.numbers, st.header, st.margins);
            a.settings.save();
        }
        a.invalidate();
    }
    if outcome == super::preview::Outcome::Print {
        print_tab(cell);
    }
}

/// File → Print: the system's print dialog, a question first when it's a lot of pages, then the pages printed on
/// another thread (`App::start_print`).
fn print_tab(cell: &Cell) {
    let (hwnd, has_sel, busy, counted, title) = {
        let a = cell.borrow();
        let t = a.tab();
        (a.hwnd, !t.view.sel.is_empty(), t.task.is_some() || t.save.is_some(), t.doc.line_count().is_some(), t.title())
    };
    if busy {
        cell.borrow_mut().flash("Still working on the previous task (Esc cancels it).", true);
        return;
    }
    if !counted {
        cell.borrow_mut().flash("Print works once the file's lines are counted (a moment).", true);
        return;
    }
    // (test mode never shows the dialog)
    if super::win::SCRIPTED.with(|s| s.borrow_mut().as_mut().map(|s| s.asked.push("Print".into())).is_some()) {
        return;
    }
    let Some(choice) = ({
        let mut last = std::mem::take(&mut cell.borrow_mut().print_last);
        let c = super::print::dialog(hwnd, has_sel, &mut last);
        cell.borrow_mut().print_last = last;
        c
    }) else {
        return;
    };
    let estimate = {
        let a = cell.borrow();
        let t = a.tab();
        let (from, to) = if choice.selection { (t.view.sel.start(), t.view.sel.end()) } else { (0, t.doc.len()) };
        let lines = t.doc.line_of(to).unwrap_or(0) - t.doc.line_of(from).unwrap_or(0) + 1;
        let all = crate::print::estimate_pages(to - from, lines);
        match &choice.pages {
            Some(r) => all.min(r.iter().map(|&(a, b)| (b.saturating_sub(a) + 1) as u64).sum()),
            None => all,
        }
    };
    if estimate > crate::print::PAGES_MAX {
        choice.printer.discard();
        let q = format!("That's about {} pages", group(estimate));
        let more = format!("Slate prints up to {} pages at a time. Select the part to print, then choose Print and Selection.", group(crate::print::PAGES_MAX));
        win::ask(hwnd, "Slate", &q, &more, &["OK"]);
        return;
    }
    if estimate > crate::print::PAGES_ASK {
        let q = format!("Print about {} pages?", group(estimate));
        if win::ask(hwnd, "Slate", &q, "", &["&Print", "Cancel"]) != Some(0) {
            choice.printer.discard();
            return;
        }
    }
    // (on this thread: a printer's own dialog, like Print to PDF's Save as, belongs to the window)
    let Some(printer) = choice.printer.start(&title, None) else { return };
    cell.borrow_mut().start_print(printer, choice.selection, choice.pages);
}

pub fn run_cmd(cell: &Cell, cmd: Cmd) {
    match cmd {
        Cmd::Open => {
            let (hwnd, dir) = {
                let a = cell.borrow();
                (a.hwnd, a.tab().doc.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).or_else(|| recent_folder(&a)))
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
            let (ids, active) = {
                let a = cell.borrow();
                (a.tabs.iter().filter(|t| t.doc.is_dirty()).map(|t| t.id).collect::<Vec<u64>>(), a.tab().id)
            };
            for id in ids {
                let Some(i) = tab_index(cell, id) else { continue };
                let (dirty, named) = {
                    let t = &cell.borrow().tabs[i];
                    (t.doc.is_dirty(), t.doc.path.is_some())
                };
                if !dirty {
                    continue;
                }
                if !named {
                    // The Save As dialog is about this tab: show it.
                    cell.borrow_mut().activate(i);
                }
                if !save_tab(cell, i, false, false) {
                    break;
                }
            }
            // Back to the tab the user was in (what they type next belongs there).
            if let Some(i) = tab_index(cell, active) {
                if cell.borrow().active != i {
                    cell.borrow_mut().activate(i);
                }
            }
        }
        Cmd::CloseTab | Cmd::CloseTabAt(_) => {
            let i = if let Cmd::CloseTabAt(i) = cmd { i } else { cell.borrow().active };
            {
                // (the only tab, with Settings → Close Slate with the last tab on: Slate closes once it's gone)
                let mut a = cell.borrow_mut();
                let last = a.settings.close_with_last_tab && a.tabs.len() == 1;
                a.quit_with_tab = a.tabs.get(i).filter(|_| last).map(|t| t.id);
            }
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
        Cmd::CloseAll | Cmd::CloseSaved => {
            // One by one as Close tab does it (asking about unsaved changes; Cancel stops). Close saved leaves the
            // tabs with unsaved changes, and those still busy opening, saving or converting.
            let ids: Vec<u64> = cell
                .borrow()
                .tabs
                .iter()
                .filter(|t| cmd == Cmd::CloseAll || !(t.doc.is_dirty() || t.busy()))
                .map(|t| t.id)
                .collect();
            for id in ids {
                let Some(i) = tab_index(cell, id) else { continue };
                if !close_tab(cell, i) {
                    break;
                }
            }
        }
        Cmd::Print => print_tab(cell),
        Cmd::PrintPreview => preview_tab(cell),
        Cmd::PageSetup => page_setup(cell),
        Cmd::Exit => close_window(cell),
        Cmd::Copy | Cmd::Cut => {
            let (size, hwnd) = {
                let a = cell.borrow();
                (a.copy_size(), a.hwnd)
            };
            if size > BIG_CLIPBOARD {
                let q = format!("Copy {} to the clipboard?", format_size(size));
                let detail = "That much text needs a lot of memory and can make other programs slow when they paste it.";
                if win::ask(hwnd, "Slate", &q, detail, &["&Copy", "Cancel"]) != Some(0) {
                    return;
                }
            }
            cell.borrow_mut().exec(cmd);
        }
        Cmd::ReopenEncoding(_) | Cmd::Reload => {
            let (id, dirty, saving, hwnd, title) = {
                let a = cell.borrow();
                (a.tab().id, a.tab().doc.is_dirty(), a.tab().save.is_some(), a.hwnd, a.tab().title())
            };
            if saving {
                // (said in a box: the status bar shows the save going on)
                let q = format!("{title} is being saved.");
                win::ask(hwnd, "Slate", &q, "Reload it once that's done.", &["OK"]);
                return;
            }
            if dirty {
                let q = format!("Reload {title} and lose your changes?");
                if win::ask(hwnd, "Slate", &q, "", &["&Reload", "Cancel"]) != Some(0) {
                    return;
                }
                // The prompt let other things happen (a file opened meanwhile can have become the tab shown): the tab
                // asked about is the one reloaded, if it's still there and not being saved now.
                let Some(i) = tab_index(cell, id) else { return };
                let mut a = cell.borrow_mut();
                if a.tabs[i].save.is_some() {
                    return;
                }
                a.activate(i);
            }
            cell.borrow_mut().exec(cmd);
        }
        Cmd::About => {
            let hwnd = cell.borrow().hwnd;
            let text = format!(
                "Slate {}\n\nA fast, simple text editor that opens files of any size. MIT license.\n\nSettings and unsaved work are kept in\n{}",
                super::crash::version(),
                data_dir().display()
            );
            win::info(hwnd, "About Slate", &text);
        }
        Cmd::Shortcuts => {
            // In a tab of their own, in the text's font: the columns line up, and Ctrl+F finds a key.
            let mut a = cell.borrow_mut();
            let title = "Keyboard shortcuts";
            if let Some(i) = a.tabs.iter().position(|t| t.title_override.as_deref() == Some(title)) {
                a.activate(i);
                return;
            }
            let blank = a.tabs.len() == 1 && a.tabs[0].is_blank();
            let mine = a.keymap.summary();
            let text = if mine.is_empty() {
                SHORTCUTS.to_string()
            } else {
                format!("{SHORTCUTS}\n\nYour own keys (Settings → Change keyboard shortcuts…), in place of those above\n{mine}")
            };
            let mut doc = Document::from_text(text.as_bytes());
            doc.eol = Eol::Lf;
            let mut i = a.add_tab(doc);
            // (an untouched empty tab gives way to them, as to a file opened)
            if blank && i == 1 {
                a.tabs.remove(0);
                a.active = 0;
                i = 0;
            }
            let t = &mut a.tabs[i];
            t.title_override = Some(title.into());
            t.untitled = 0;
            // (a list to read: a key typed there by mistake doesn't make it a tab with unsaved changes)
            t.read_only = true;
            a.activate(i);
        }
        Cmd::ChangeShortcuts => {
            // (made the first time, listing every command with its keys; test mode only in its own data folder)
            let p = crate::shortcuts::path();
            if win::testing() && std::env::var_os("SLATE_DATA_DIR").is_none() {
                return;
            }
            if !p.exists() {
                crate::settings::make_data_dir();
                if let Err(e) = std::fs::write(&p, crate::shortcuts::template()) {
                    cell.borrow_mut().flash(format!("Couldn't make {}: {e}", p.display()), true);
                    return;
                }
            }
            cell.borrow_mut().open_paths(&[p]);
        }
        Cmd::MakeDefault => super::install::make_default(cell),
        Cmd::StopDefault => super::install::stop_default(cell),
        other => {
            let mut a = cell.borrow_mut();
            a.exec(other);
        }
    }
}

fn tab_index(cell: &Cell, id: u64) -> Option<usize> {
    cell.borrow().tabs.iter().position(|t| t.id == id)
}

/// A save of tab `id` stopped before writing because ANSI can't hold some characters (like Notepad, ask first):
/// save it as UTF-8 instead (carrying on with closing, if that's what it was for), save as ANSI anyway (the
/// characters become "?"; the tab then stays open, with the text still in it), or don't save.
fn ask_lossy(cell: &Cell, id: u64) {
    let (hwnd, title, (path, close_after, closing, enc), damaged, suspect) = {
        let mut a = cell.borrow_mut();
        let hwnd = a.hwnd;
        let Some(t) = a.tabs.iter_mut().find(|t| t.id == id) else { return };
        let Some(ask) = t.ask_lossy.take() else { return };
        let damaged = (t.doc.bad_units > 0).then_some((t.doc.bad_units, t.doc.encoding));
        let suspect = t.doc.suspect;
        (hwnd, t.title(), ask, damaged, suspect)
    };
    if suspect {
        // Text was put in after part of the file couldn't be read: it may hold the blank bytes that read gave.
        let q = format!("Part of {title} couldn't be read while it was edited");
        let detail = "An edit since may have copied blank bytes from where it couldn't be read (a network drive that \
                      dropped, or a part another program had locked), and saving would write them over what the file \
                      has. Reload it from disk to start again from what's there now.";
        let choice = win::ask(hwnd, "Slate", &q, detail, &["&Save anyway", "Cancel"]);
        let Some(i) = tab_index(cell, id) else { return };
        if choice == Some(0) {
            let started = cell.borrow_mut().start_save_lossy(i, path, enc, close_after, true);
            if started && closing {
                run_cmd(cell, Cmd::Exit);
            }
        }
        return;
    }
    if let Some((n, enc)) = damaged {
        // A file that wasn't all text in its encoding: what wasn't shows as U+FFFD, and can't be saved back.
        let q = format!("Parts of {title} weren't {} text", enc.label());
        let what = if n == 1 { "One place shows".to_string() } else { format!("{n} places show") };
        let detail = format!("{what} \"\u{FFFD}\" where the file had something else, and would be saved that way.");
        let choice = win::ask(hwnd, "Slate", &q, &detail, &["&Save anyway", "Cancel"]);
        let Some(i) = tab_index(cell, id) else { return };
        if choice == Some(0) {
            let enc = cell.borrow().tabs[i].doc.encoding;
            let started = cell.borrow_mut().start_save_lossy(i, path, enc, close_after, true);
            if started && closing {
                run_cmd(cell, Cmd::Exit);
            }
        }
        return;
    }
    let (q, detail, anyway) = if enc == Encoding::Ansi {
        (
            format!("Some characters in {title} can't be saved as ANSI"),
            format!(
                "In {} they would become \"?\". UTF-8 keeps every character, and nearly every program reads it.",
                Encoding::Ansi.label()
            ),
            "Save as &ANSI anyway".to_string(),
        )
    } else {
        // UTF-16 (bytes that aren't text, in a file that was mostly UTF-8, have no UTF-16 form)
        (
            format!("Parts of {title} aren't text and can't be saved as {}", enc.label()),
            "In UTF-16 they would become \"\u{FFFD}\". UTF-8 keeps them as they are.".to_string(),
            format!("Save as {} &anyway", enc.label()),
        )
    };
    let choice = win::ask(hwnd, "Slate", &q, &detail, &["Save as &UTF-8", &anyway, "Cancel"]);
    // The dialog let other things happen (tabs can close or move meanwhile): find the tab again.
    let Some(i) = tab_index(cell, id) else { return };
    match choice {
        Some(0) => {
            let started = {
                let mut a = cell.borrow_mut();
                let t = &mut a.tabs[i];
                t.doc.encoding = Encoding::Utf8;
                t.doc.bom = false;
                a.start_save(i, path, Encoding::Utf8, close_after)
            };
            if started && closing {
                run_cmd(cell, Cmd::Exit);
            }
        }
        Some(1) => {
            cell.borrow_mut().start_save_lossy(i, path, enc, false, true);
        }
        _ => {}
    }
}

/// Where the Open and Save As boxes start for a tab without a file: the folder of the file opened or saved last (Open
/// recent), unless it's one of Slate's own (its data or program folder); else (`win::start_in`) Documents.
fn recent_folder(a: &App) -> Option<PathBuf> {
    let own = |d: &Path| d.starts_with(data_dir()) || d.starts_with(super::install::install_dir());
    a.settings.recent.iter().filter_map(|p| p.parent()).find(|d| !own(d)).map(Path::to_path_buf)
}

/// Saves tab `i` (asking for a name when needed). Returns false if the user cancelled or it can't be saved now.
pub fn save_tab(cell: &Cell, i: usize, ask_name: bool, close_after: bool) -> bool {
    let (id, path, title, hwnd, recent) = {
        let a = cell.borrow();
        let Some(t) = a.tabs.get(i) else { return false };
        (t.id, t.doc.path.clone(), t.title(), a.hwnd, recent_folder(&a))
    };
    let path = match path {
        Some(p) if !ask_name => p,
        other => {
            let name = if other.is_some() { title.clone() } else { format!("{title}.txt") };
            let dir = other.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf).or(recent);
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
    let (id, dirty, saving_same, title, hwnd, waiting) = {
        let a = cell.borrow();
        let Some(t) = a.tabs.get(i) else { return true };
        let dirty = t.doc.is_dirty() && !(t.doc.is_empty() && t.doc.path.is_none());
        let waiting = t.restore.as_ref().is_some_and(Restoring::is_big);
        (t.id, dirty, t.save.as_ref().is_some_and(|s| s.version == t.doc.version), t.title(), a.hwnd, waiting)
    };
    if waiting {
        // Unsaved changes from last time that aren't back yet: closing loses them.
        let q = format!("Close {title} and lose its unsaved changes from last time?");
        let detail = "They aren't back yet: Slate is putting them back, or waiting for the file to answer.";
        if win::ask(hwnd, "Slate", &q, detail, &["&Close and lose them", "Cancel"]) != Some(0) {
            return false;
        }
        if let Some(i) = tab_index(cell, id) {
            cell.borrow_mut().remove_tab(i);
        }
        return true;
    }
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
        let choice = win::ask(hwnd, "Slate", &q, "", &["&Save", "Do&n't save", "Cancel"]);
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
    // (files another Slate hands over meanwhile wait: they'd go down with the window)
    let _closing = super::Closing::now();
    // Keep what can be kept first; whatever that doesn't cover is asked about.
    let session_ok = cell.borrow_mut().save_session();
    let restore = cell.borrow().settings.restore_session;
    // Not keeping the session (it's turned off): its files go, also those of tabs from last time that aren't back.
    if !restore {
        let big = |t: &&Tab| t.restore.as_ref().is_some_and(Restoring::is_big);
        let waiting: Vec<u64> = cell.borrow().tabs.iter().filter(big).map(|t| t.id).collect();
        for id in waiting {
            let Some(i) = tab_index(cell, id) else { continue };
            cell.borrow_mut().activate(i);
            if !close_tab(cell, i) {
                cell.borrow_mut().cancel_close();
                return;
            }
        }
    }
    let ids = cell.borrow().unkept_tabs(session_ok);
    let mut saving = false;
    for id in ids {
        let Some(i) = tab_index(cell, id) else { continue };
        let (title, hwnd, why) = {
            let a = cell.borrow();
            (a.tabs[i].title(), a.hwnd, session::unkept_reason(&a.tabs[i]))
        };
        cell.borrow_mut().activate(i);
        let detail = if super::settings::guest() {
            "This window opened while another Slate was busy, so it doesn't keep unsaved changes for next time."
        } else if !restore {
            ""
        } else if !session_ok {
            "Slate couldn't keep unsaved changes for next time (the settings folder can't be written)."
        } else {
            why
        };
        let q = format!("Do you want to save changes to {title}?");
        let choice = win::ask(hwnd, "Slate", &q, detail, &["&Save", "Do&n't save", "Cancel"]);
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
        a.remember_places();
        a.end_waits();
        if restore {
            a.save_session();
        } else {
            session::clear();
        }
        a.settings.save();
        // (a start that never drew its window, minimized say, didn't crash while restoring)
        if !a.g.offscreen {
            session::restored();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_after_the_file_name() {
        let p = |s: &str| line_suffix(Path::new(s));
        assert_eq!(p(r"C:\work\notes.txt:120"), Some((PathBuf::from(r"C:\work\notes.txt"), 120, None)));
        assert_eq!(p(r"C:\work\notes.txt:120:5"), Some((PathBuf::from(r"C:\work\notes.txt"), 120, Some(5))));
        // not a line: no number, nothing before it, a drive
        assert_eq!(p(r"C:\work\notes.txt"), None);
        assert_eq!(p(r"C:\work\notes.txt:"), None);
        assert_eq!(p(r"C:\work\notes.txt:x"), None);
        assert_eq!(p(r"C:\work\:12"), None);
        assert_eq!(p(r"C:"), None);
        assert_eq!(p(r"C:\"), None);
        // a stream name before the number stays part of the name
        assert_eq!(p(r"C:\w\a.txt:s:7"), Some((PathBuf::from(r"C:\w\a.txt:s"), 7, None)));
    }

    #[test]
    fn columns_are_counted_in_bulk() {
        // as the character-by-character walk counted them, over text made of several pieces
        let mut d = Document::from_text("aé€😀 x\t".as_bytes());
        d.begin(EditKind::Other, Sel::at(0));
        d.insert(3, "日本".as_bytes());
        d.insert(0, "zz".as_bytes());
        d.end(Sel::at(0));
        let len = d.len();
        let mut walk = 0;
        for n in 0..20 {
            assert_eq!(skip_chars(&d, 0, len, n), walk, "{n} characters");
            walk = d.next_char(walk).min(len);
        }
        // at most the end given
        assert_eq!(skip_chars(&d, 0, 4, 99), 4);
        assert_eq!(skip_chars(&d, 2, len, 0), 2);
    }

    #[test]
    fn lines_and_columns_to_positions() {
        let d = Document::from_text("ab\r\ncdé\nlast".as_bytes());
        assert_eq!(line_col_pos(&d, 1, None), Some(0));
        assert_eq!(line_col_pos(&d, 2, Some(3)), Some(6));
        // past the end of the line, or of the document: the end of the line, the last line
        assert_eq!(line_col_pos(&d, 2, Some(99)), Some(8));
        assert_eq!(line_col_pos(&d, 99, None), Some(9));
        assert_eq!(line_col_pos(&d, 0, None), Some(0));
    }
}
