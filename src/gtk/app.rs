//! The Linux window's state and what it does: tabs and their documents, opening and saving (on other threads,
//! through the shared engine), the editing keys, find and replace, line tools and JSON/XML. What needs a dialog
//! (open, save as, an unsaved tab closing, go to line) is queued in `asks` for the window to show; the answers come
//! back as calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::gdk;

use crate::core::buffer::Snapshot;
use crate::core::document::{Document, EditKind, Sel};
use crate::core::io::{self as fileio, Loading, Opened, SaveError, Saved};
use crate::core::job::{Job, Notify};
use crate::core::search::{self, Found, Matcher, Query};
use crate::core::source::IndexBuilder;
use crate::core::text::{Encoding, Eol};
use crate::core::{csv, json, lines, xml};
use crate::edit::{self, Indent, Sink};
use crate::highlight::Lang;
use crate::settings::{Settings, ThemeMode, ZOOM_STEPS};
use crate::theme::Theme;

use super::session;
use super::view::{Ctx, Geom, Style, View};

/// Documents up to this size are kept in the session as a copy; bigger unsaved ones are asked about on closing.
pub const KEEP_MAX: u64 = 64 << 20;
/// Line tools on the whole document work up to this size.
const LINES_MAX: u64 = 512 << 20;
/// A selection the line tools work on in place.
const SELECTION_MAX: u64 = 16 << 20;
/// How long a message shows in the status bar (as on Windows); then the caret's place again.
const FLASH_FOR: Duration = Duration::from_secs(6);

/// What the window shows for the app (a dialog, the file chooser...), queued while the app is busy.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    Open,
    SaveAs { tab: u64, close_after: bool },
    /// The tab has unsaved changes Slate can't keep: save, don't save or cancel (then close it).
    CloseUnsaved { tab: u64 },
    /// Saving in ANSI would turn characters into "?" (or a damaged UTF-16 file can't be saved back as it was).
    Lossy { tab: u64, path: PathBuf, encoding: Encoding, close_after: bool },
    GotoLine,
    /// Quit once the unsaved tabs that can't be kept are dealt with.
    Quit,
    About,
    Shortcuts,
    /// Make Slate the default app for the file types it opens?
    MakeDefault,
    /// Stop Slate being the default app for them?
    StopDefault,
    /// File → Print: GTK's print dialog (after a question when it's about `pages` pages, a lot).
    Print { pages: u64 },
}

/// A menu command or shortcut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    NewTab,
    Open,
    Save,
    SaveAs,
    SaveAll,
    Print,
    CloseTab,
    ReopenClosed,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    Replace,
    FindNext,
    FindPrev,
    ReplaceOne,
    ReplaceAll,
    CloseFind,
    GotoLine,
    ToggleComment,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    Lines(lines::LineOp),
    Case(lines::CaseOp),
    /// Format JSON or XML, whichever the document is (Shift+Alt+F).
    Format,
    JsonFormat,
    JsonMinify,
    JsonCheck,
    CsvCheck,
    /// The rows sorted by the caret's column (Z to A: true).
    CsvSort(bool),
    XmlFormat,
    XmlMinify,
    XmlCheck,
    Wrap,
    /// View → Character at the caret: its code point and bytes in the status bar.
    CharInfo,
    LineNumbers,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Theme(ThemeMode),
    NextTab,
    PrevTab,
    GoTab(usize),
    SetEol(Eol),
    SetLang(Lang),
    InsertDateTime,
    About,
    Shortcuts,
    MakeDefault,
    StopDefault,
    /// File → Read only: the tab's text can't be changed.
    ToggleReadOnly,
    /// View → Show only matching lines (false) / lines not matching (true): the find bar's search, the lines in a tab
    /// of their own.
    FilterLines(bool),
    /// Bookmarks: the caret's line marked or not (Ctrl+F2), the next and previous (F2, Shift+F2), none.
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,
}

pub struct SaveTask {
    job: Job<Result<Saved, SaveError>>,
    state: u64,
    version: u64,
    doc: u64,
    path: PathBuf,
    encoding: Encoding,
    close_after: bool,
}

enum TaskResult {
    Content { src: Arc<crate::core::source::Source>, nl: u64, count: u64 },
    /// The lines kept by a filter, for a tab of their own.
    Filtered { src: Arc<crate::core::source::Source>, nl: u64, found: crate::core::search::Filtered, query: String },
    Valid(String),
    FormatError { offset: u64, msg: String },
    Failed(String),
    Cancelled,
}

impl crate::core::job::Failure for TaskResult {
    fn failure(msg: &str) -> Self {
        TaskResult::Failed(msg.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TaskKind {
    Json(json::Mode),
    Xml(json::Mode),
    Lines(lines::LineOp),
    ReplaceAll,
    /// Every line break to this kind.
    Eol(Eol),
    /// Show only matching lines (false), or the lines without a match (true).
    Filter(bool),
    /// CSV or TSV: Check.
    CsvCheck(csv::Dialect),
    /// CSV or TSV: the rows sorted by field `col` (from 0), Z to A when `desc`, the first row on top when `header`.
    SortColumn { col: usize, desc: bool, d: csv::Dialect, header: bool },
}

struct Task {
    kind: TaskKind,
    job: Job<TaskResult>,
    version: u64,
}

pub struct Tab {
    pub id: u64,
    pub doc: Document,
    pub view: View,
    pub lang: Lang,
    /// The user picked the language (it's not detected again on saving under another name).
    pub lang_picked: bool,
    pub indent: Indent,
    load: Option<Job<Opened>>,
    convert: Option<Job<std::io::Result<Document>>>,
    index: Option<Job<bool>>,
    save: Option<SaveTask>,
    task: Option<Task>,
    /// Shown above the text: why it can't be edited, that its file changed...
    pub notice: Option<String>,
    /// The notice is about a save that failed: it offers Save as….
    pub notice_save_as: bool,
    /// Line (and column) to go to once the document is there (`slate file.txt:120`).
    pub goto: Option<(u64, u64)>,
    /// What was last seen on disk (a change already told about isn't told again).
    pub seen_disk: Option<Option<crate::core::document::DiskInfo>>,
    /// Where the session keeps its text (its backup file's name), when it does.
    pub backup: Option<String>,
    /// The selection (anchor, caret) and top to go back to once its file is read (a tab from the session).
    pub restore_at: Option<(u64, u64, u64)>,
    /// The document version the session's copy of its text is of (it isn't written again while that's so).
    pub session_version: u64,
    /// The canonical path found when its file was read (another open of the same file comes to this tab).
    pub canon: Option<PathBuf>,
    /// What the tab is called when it isn't a file's (Keyboard shortcuts).
    pub title_override: Option<String>,
    /// Being read again after another program changed the file: whether the caret was at the end (it stays there,
    /// following a log as it grows).
    reload: Option<bool>,
    /// File → Read only (on by itself for a file this user can't write): the text can't be changed here.
    pub read_only: bool,
    /// A tab of matching lines (View → Show only matching lines): read only, not kept for next time.
    pub filter: Option<FilterOf>,
    /// The bookmarks to put back once its file is read (a tab from the session).
    pub restore_marks: Option<Vec<u64>>,
}

/// What a tab of matching lines shows: lines of the tab `source`, `map` saying which line there each line here is.
pub struct FilterOf {
    pub source: u64,
    pub map: Arc<crate::core::search::LineMap>,
}

impl Tab {
    pub fn title(&self) -> String {
        if let Some(t) = &self.title_override {
            return t.clone();
        }
        match &self.doc.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None => "Untitled".into(),
        }
    }

    pub fn busy(&self) -> bool {
        self.load.is_some() || self.convert.is_some() || self.save.is_some() || self.task.is_some()
    }

    pub fn loading(&self) -> bool {
        self.load.is_some() || self.convert.is_some()
    }

    pub fn saving(&self) -> bool {
        self.save.is_some()
    }

    /// CSV and TSV: the caret's field ("status (4)") for the status bar.
    pub fn field_text(&self) -> Option<String> {
        if !matches!(self.lang, Lang::Csv | Lang::CsvSemi | Lang::Tsv) || !self.doc.is_ready() {
            return None;
        }
        let d = edit::csv_dialect(self.lang, &self.doc)?;
        edit::csv_field(&self.doc, d, self.view.sel.caret).map(|(col, name)| edit::csv_field_text(col, name.as_deref()))
    }

    /// An untouched empty tab (a new window's): what's opened next takes its place.
    pub fn is_blank(&self) -> bool {
        self.doc.path.is_none() && self.doc.is_empty() && !self.doc.is_dirty() && !self.busy()
    }
}

/// Find and replace.
#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub replace: bool,
    pub query: Query,
    pub replacement: String,
    pub matcher: Option<Matcher>,
    pub error: Option<String>,
    /// All matches of the query in the active document (when counted), for which version.
    pub found: Option<(u64, Found)>,
    count_job: Option<(u64, Job<Found>)>,
    /// The tab the last count was for.
    count_tab: u64,
    find_job: Option<(u64, bool, Sel, Job<Option<(u64, u64)>>)>,
    /// Up and Down in a box go through its history: where they are in it (None: the text typed, kept in
    /// `history_typed` meanwhile).
    pub history_at: Option<usize>,
    history_typed: String,
    /// What was selected when the bar opened, and the match the search selected last (see `toggle_in_sel`).
    sel_before: Option<(u64, u64)>,
    last_match: Option<(u64, u64)>,
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub settings: Settings,
    pub theme: Theme,
    pub style: Style,
    pub notify: Notify,
    /// What the look at the files on disk wakes when it's done (`Ui::poll_disk`: a refresh only if it found something).
    pub disk_notify: Option<Notify>,
    /// The last look at the files on disk found something to show.
    pub disk_found: bool,
    pub find: Find,
    pub asks: Vec<Ask>,
    /// A message in the status bar (and whether it's bad news), shown for `FLASH_FOR` from `flash_at`.
    pub flash: Option<(String, bool)>,
    pub flash_at: Instant,
    pub caret_on: bool,
    pub focused: bool,
    /// Files of tabs closed lately, for Reopen closed tab.
    pub closed: Vec<PathBuf>,
    /// The system wants dark (when the theme follows it).
    pub system_dark: bool,
    /// The window should be drawn again / its title and menus updated.
    pub dirty_view: bool,
    pub dirty_title: bool,
    /// Text for the clipboard, and a paste asked for (the window does both: GTK's clipboard is the window's).
    pub copy_out: Option<String>,
    pub paste_wanted: bool,
    /// The whole line copied with nothing selected: pasting just that puts it above the caret's line.
    copied_line: Option<String>,
    pub quitting: bool,
    pub session_dirty: bool,
    /// The session being written on another thread (`session::start`).
    pub session_job: Option<Job<session::Outcome>>,
    /// Closing couldn't write the session: every unsaved tab is asked about.
    pub session_failed: bool,
    /// The caret should be shown (scrolled to) once the text is laid out; `reveal_center`: in the middle.
    pub reveal_pending: bool,
    pub reveal_center: bool,
    /// The font changed (zoom): the style is made again.
    pub restyle: bool,
    next_id: u64,
    disk_job: Option<Job<Vec<(u64, Option<crate::core::document::DiskInfo>)>>>,
    /// The status bar's column (document version, caret, column) and selection size (version, start, end,
    /// characters), counted once for each.
    pub col_cache: std::cell::Cell<Option<(u64, u64, u64)>>,
    pub sel_cache: std::cell::Cell<Option<(u64, u64, u64, u64)>>,
    /// CSV and TSV: the caret's field as the status bar says it, for (tab, document version, caret).
    pub field_cache: std::cell::RefCell<Option<((u64, u64, u64), Option<String>)>>,
    /// `slate --wait`: a wait file to delete once all of its tabs are closed (`open_hinted`).
    pub waits: Vec<(PathBuf, Vec<u64>)>,
}

fn now_text() -> String {
    // (local time without a time zone library: what `date` gives)
    std::process::Command::new("date")
        .arg("+%H:%M %Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

impl App {
    pub fn new(settings: Settings, style: Style, notify: Notify, system_dark: bool) -> App {
        let mut app = App {
            tabs: Vec::new(),
            active: 0,
            theme: Theme::light(crate::theme::rgb(0x0067C0)),
            settings,
            style,
            notify,
            disk_notify: None,
            disk_found: false,
            find: Find::default(),
            asks: Vec::new(),
            flash: None,
            flash_at: Instant::now(),
            caret_on: true,
            focused: true,
            closed: Vec::new(),
            system_dark,
            dirty_view: true,
            dirty_title: true,
            copy_out: None,
            paste_wanted: false,
            copied_line: None,
            quitting: false,
            session_dirty: false,
            session_job: None,
            session_failed: false,
            reveal_pending: false,
            reveal_center: false,
            restyle: false,
            next_id: 1,
            disk_job: None,
            col_cache: Default::default(),
            sel_cache: Default::default(),
            field_cache: Default::default(),
            waits: Vec::new(),
        };
        app.apply_theme();
        app
    }

    pub fn dark(&self) -> bool {
        match self.settings.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => self.system_dark,
        }
    }

    pub fn apply_theme(&mut self) {
        let accent = crate::theme::rgb(0x3584E4);
        self.theme = if self.dark() { Theme::dark(accent) } else { Theme::light(accent) };
        for t in &mut self.tabs {
            t.view.clear_cache();
        }
        self.dirty_view = true;
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn flash(&mut self, msg: impl Into<String>, bad: bool) {
        self.flash = Some((msg.into(), bad));
        self.flash_at = Instant::now();
        self.dirty_view = true;
    }

    /// What the status bar says in place of the caret's place: a recent message, else the work going on.
    pub fn status_message(&self) -> Option<(String, bool)> {
        if let Some(f) = self.flash.as_ref().filter(|_| self.flash_at.elapsed() < FLASH_FOR) {
            return Some(f.clone());
        }
        if self.tab().task.is_some() {
            return Some(("Working…".into(), false));
        }
        if self.find.find_job.is_some() {
            return Some(("Searching…".into(), false));
        }
        None
    }

    /// Takes away a message that has shown long enough (the window's 2-second timer asks).
    /// Takes away a message shown for `FLASH_FOR`; returns whether it did (the window shows the caret's place again).
    pub fn expire_flash(&mut self) -> bool {
        if self.flash.is_some() && self.flash_at.elapsed() >= FLASH_FOR {
            self.flash = None;
            return true;
        }
        false
    }

    fn make_tab(&mut self, doc: Document) -> Tab {
        let id = self.next_id;
        self.next_id += 1;
        let indent = default_indent(&self.settings);
        Tab {
            id,
            doc,
            view: View::new(),
            lang: Lang::Plain,
            lang_picked: false,
            indent,
            load: None,
            convert: None,
            index: None,
            save: None,
            task: None,
            notice: None,
            notice_save_as: false,
            goto: None,
            seen_disk: None,
            backup: None,
            restore_at: None,
            session_version: 0,
            canon: None,
            title_override: None,
            reload: None,
            read_only: false,
            filter: None,
            restore_marks: None,
        }
    }

    pub fn new_untitled(&mut self) -> usize {
        let tab = self.make_tab(Document::new());
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.dirty_title = true;
        self.dirty_view = true;
        self.active
    }

    /// A new tab with `text` (from the session), unsaved, its file `path` if it has one.
    pub fn add_text_tab(&mut self, text: &[u8], path: Option<PathBuf>, lang: Option<Lang>) -> usize {
        let mut doc = Document::from_text(text);
        doc.path = path.clone();
        doc.mark_dirty();
        let mut tab = self.make_tab(doc);
        tab.lang = lang.unwrap_or_else(|| detect(path.as_deref(), &tab.doc));
        if let Some(i) = edit::detect_indent(text) {
            tab.indent = i;
        }
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Opens files (or switches to the tab that has one open already); an untouched empty tab gives way to the
    /// first.
    pub fn open_paths(&mut self, paths: &[PathBuf]) {
        // (the tab shown changes: a message about the one before goes; see `open_handed`)
        if !paths.is_empty() {
            self.flash = None;
        }
        for p in paths {
            // `notes.txt:120:5`: that file at line 120, when there's no file of that very name. The open finds out,
            // on another thread, as the file system isn't asked here: a network drive that doesn't answer would
            // freeze the window. (Another name for a file open already is found once it's read: `opened`.)
            let suffix = line_suffix(p);
            let names = [Some(p.as_path()), suffix.as_ref().map(|s| s.0.as_path())];
            if let Some(i) = self.tabs.iter().position(|t| t.doc.path.as_deref().is_some_and(|q| names.contains(&Some(q)))) {
                self.active = i;
                if let Some((base, line, col)) = &suffix {
                    if self.tabs[i].doc.path.as_deref() == Some(base.as_path()) {
                        self.tabs[i].goto = Some((*line, *col));
                        self.apply_goto(i);
                    }
                }
                continue;
            }
            let reuse = self.tabs.len() == 1 && self.tabs[0].is_blank();
            let mut doc = Document::new();
            doc.path = Some(p.clone());
            let mut tab = self.make_tab(doc);
            let notify = self.notify.clone();
            let full = p.clone();
            tab.load = Some(match suffix {
                Some((base, line, col)) => {
                    // (the line applies if `base` is what's read: `opened`)
                    tab.goto = Some((line, col));
                    Job::spawn(fileio::OPEN_STEPS, notify.clone(), move |ctx| fileio::open_either(&full, &base, notify, true, ctx))
                }
                None => Job::spawn(fileio::OPEN_STEPS, notify.clone(), move |ctx| fileio::open(&full, notify, true, ctx)),
            });
            if reuse {
                self.tabs[0] = tab;
                self.active = 0;
            } else {
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
            }
        }
        self.dirty_title = true;
        self.dirty_view = true;
    }

    /// Opens files named on a command line (Slate's own, or another start's handed over): a message from before stays
    /// (what putting the session back said at the start), unless opening these says something itself.
    pub fn open_handed(&mut self, paths: &[PathBuf]) {
        let said = self.flash.take();
        self.open_paths(paths);
        if self.flash.is_none() {
            self.flash = said;
        }
    }

    /// What another start handed over with a hint (`slate:read-only;wait=<token>;text=<token>;`: `mod::run_request`):
    /// standard input's text in a new tab (its file in the data folder's `handed` folder, read and deleted), the files
    /// read only if asked, and `--wait`'s tabs noted (its wait file goes once they're all closed: `tab_gone`). Any
    /// other hint: the files as they are.
    pub fn open_hinted(&mut self, paths: &[PathBuf], hint: &str) {
        let Some(rest) = hint.strip_prefix("slate:") else { return self.open_handed(paths) };
        let (mut read_only, mut wait, mut text) = (false, None, None);
        for part in rest.split(';') {
            match part.split_once('=') {
                Some(("wait", t)) => wait = Some(t.to_string()),
                Some(("text", t)) => text = Some(t.to_string()),
                _ if part == "read-only" => read_only = true,
                _ => {}
            }
        }
        let handed = crate::settings::data_dir().join("handed");
        // (a token is `<pid>-<time>`: nothing that names a file outside that folder)
        let ours = |t: &String| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || b == b'-');
        let text_file = text.filter(ours).map(|t| handed.join(format!("stdin-{t}.txt")));
        let before: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        let files: Vec<PathBuf> = paths.iter().filter(|p| Some(*p) != text_file.as_ref()).cloned().collect();
        let said = self.flash.take();
        if let Some(f) = &text_file {
            if let Ok(bytes) = std::fs::read(f) {
                let blank = self.tabs.len() == 1 && self.tabs[0].is_blank();
                let i = self.add_text_tab(&bytes, None, None);
                self.active = i;
                if blank {
                    self.remove_tab(0);
                }
            }
            let _ = std::fs::remove_file(f);
        }
        if !files.is_empty() {
            self.open_paths(&files);
        }
        if self.flash.is_none() {
            self.flash = said;
        }
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| !before.contains(&t.id) || t.doc.path.as_ref().is_some_and(|p| files.contains(p)))
            .map(|t| t.id)
            .collect();
        if read_only {
            for t in self.tabs.iter_mut().filter(|t| ids.contains(&t.id)) {
                t.read_only = true;
            }
        }
        if let Some(w) = wait.filter(ours) {
            let file = handed.join(format!("wait-{w}"));
            if ids.is_empty() {
                let _ = std::fs::remove_file(&file);
            } else {
                self.waits.push((file, ids));
            }
        }
        self.session_dirty = true;
        self.dirty_title = true;
        self.dirty_view = true;
    }

    /// Tab `id` closed: a `--wait` all of whose tabs are closed now is told (its wait file goes).
    fn tab_gone(&mut self, id: u64) {
        for (_, ids) in self.waits.iter_mut() {
            ids.retain(|&t| t != id);
        }
        self.waits.retain(|(file, ids)| {
            if ids.is_empty() {
                let _ = std::fs::remove_file(file);
            }
            !ids.is_empty()
        });
    }

    /// Every `--wait` is told (the window is closing).
    pub fn end_waits(&mut self) {
        for (file, _) in std::mem::take(&mut self.waits) {
            let _ = std::fs::remove_file(file);
        }
    }

    /// View → Show only matching lines: the find bar's search (opened to type one), the lines in a tab of their own.
    fn filter_lines(&mut self, invert: bool) {
        if self.find.matcher.is_none() {
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

    /// The lines a filter of tab `i` kept, in a tab of their own after it (or what it found when no line is kept).
    fn show_filtered(
        &mut self,
        i: usize,
        invert: bool,
        src: Arc<crate::core::source::Source>,
        nl: u64,
        found: crate::core::search::Filtered,
        query: &str,
    ) {
        let q: String = if query.chars().count() > 40 { query.chars().take(39).collect::<String>() + "…" } else { query.to_string() };
        if found.map.lines == 0 {
            self.flash(if invert { format!("Every line has “{q}”") } else { format!("No line has “{q}”") }, false);
            return;
        }
        // (lines of a tab of lines: of the tab those came from)
        let source = &self.tabs[i];
        let (origin, map) = match &source.filter {
            Some(f) => (f.source, found.map.through(&f.map)),
            None => (source.id, found.map),
        };
        let (lang, encoding, eol) = (source.lang, source.doc.encoding, source.doc.eol);
        let from = self.tabs.iter().find(|t| t.id == origin);
        let name = from.map_or_else(|| "the file".to_string(), |t| t.title());
        let total = from.and_then(|t| edit::text_lines(&t.doc));
        let mut doc = Document::from_buffer(crate::core::buffer::Buffer::from_source(src, nl));
        doc.encoding = encoding;
        doc.eol = eol;
        let lines = map.lines;
        let mut tab = self.make_tab(doc);
        tab.lang = lang;
        tab.lang_picked = true;
        tab.filter = Some(FilterOf { source: origin, map: Arc::new(map) });
        tab.title_override = Some(if invert { format!("not “{q}” in {name}") } else { format!("“{q}” in {name}") });
        tab.notice = Some(edit::filtered_note(lines, total, &name, &q, invert, found.cut));
        let at = (i + 1).min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.active = at;
        self.find.found = None;
        self.dirty_title = true;
        self.dirty_view = true;
    }

    /// Enter or a double-click in a tab of matching lines: the caret's line, in the tab the lines came from.
    pub fn go_to_source(&mut self) {
        let tab = self.tab();
        let Some(f) = &tab.filter else { return };
        let Some(line) = tab.doc.line_of(tab.view.sel.caret).and_then(|l| f.map.source(l)) else { return };
        let source = f.source;
        let Some(i) = self.tabs.iter().position(|t| t.id == source) else {
            self.flash("The tab these lines came from is closed.", true);
            return;
        };
        self.active = i;
        self.find.found = None;
        self.tabs[i].goto = Some((line + 1, 1));
        self.goto_now(i);
        self.dirty_title = true;
    }

    /// File → Print: GTK's print dialog for the tab (`Ask::Print`), unless it's far too long.
    fn print(&mut self) {
        let tab = self.tab();
        if tab.busy() {
            self.flash("Still working on the previous task (Esc cancels it).", true);
            return;
        }
        let Some(lines) = tab.doc.line_count() else {
            self.flash("Print works once the file's lines are counted (a moment).", true);
            return;
        };
        // (a selection can be printed instead, chosen in the dialog)
        let pages = if tab.view.sel.is_empty() { crate::print::estimate_pages(tab.doc.len(), lines) } else { 0 };
        if pages > crate::print::PAGES_MAX {
            let msg = format!("That's about {} pages: select the part to print, then print the selection.", crate::edit::group(pages));
            self.flash(msg, true);
            return;
        }
        self.asks.push(Ask::Print { pages });
    }

    /// Whether anything runs in the background (test mode waits for it).
    pub fn busy(&self) -> bool {
        self.tabs.iter().any(|t| t.busy() || t.index.is_some())
            || self.find_running()
            || self.disk_job.is_some()
            || self.session_job.is_some()
    }

    /// Goes to the line asked for (`Tab::goto`) in tab `i` now, and shows it.
    pub fn goto_now(&mut self, i: usize) {
        // The view stays where it is when the line is in it (as on Windows); revealing scrolls it there otherwise.
        let (top, row) = (self.tabs[i].view.top, self.tabs[i].view.top_row);
        self.apply_goto(i);
        self.tabs[i].view.top = top;
        self.tabs[i].view.top_row = row;
        self.reveal_pending = true;
        self.reveal_center = true;
    }

    fn apply_goto(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        if tab.loading() || !tab.doc.is_ready() {
            return;
        }
        let Some((line, col)) = tab.goto.take() else { return };
        let start = tab.doc.line_start(line.saturating_sub(1)).unwrap_or(tab.doc.len());
        let end = tab.doc.line_end_of(start);
        let pos = (start + col.saturating_sub(1)).min(end);
        tab.view.sel = Sel::at(pos);
        tab.view.want_x = None;
        tab.view.top = tab.doc.line_start_of(pos);
        tab.view.top_row = 0;
        self.dirty_view = true;
    }

    // ---- background work ----

    /// Picks up finished background work (called when a job says it's done, and by a timer while any runs).
    /// Returns whether anything still runs.
    pub fn poll_jobs(&mut self) -> bool {
        let mut running = false;
        let ids: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            if let Some(i) = self.index_of(id) {
                running |= self.poll_tab(i);
            }
        }
        running |= self.poll_find();
        running |= self.poll_disk();
        running |= session::poll(self);
        running
    }

    fn poll_tab(&mut self, i: usize) -> bool {
        let mut running = false;
        if let Some(job) = self.tabs[i].load.as_mut() {
            match job.take() {
                Some(opened) => {
                    self.tabs[i].load = None;
                    if !self.opened(i, opened) {
                        // (the tab is gone: the rest isn't its own any more)
                        return false;
                    }
                }
                None => running = true,
            }
        }
        if let Some(job) = self.tabs[i].convert.as_mut() {
            match job.take() {
                Some(Ok(doc)) => {
                    self.tabs[i].convert = None;
                    self.loaded(i, doc, None);
                }
                Some(Err(e)) => {
                    self.tabs[i].convert = None;
                    self.tabs[i].notice = Some(format!("Couldn't read this file: {}", fileio::friendly_io(&e)));
                }
                None => running = true,
            }
        }
        if let Some(job) = self.tabs[i].index.as_mut() {
            match job.take() {
                Some(_) => {
                    let tab = &mut self.tabs[i];
                    tab.index = None;
                    tab.doc.poll_index();
                    tab.view.clear_cache();
                    if let Some(why) = tab.doc.index_error() {
                        tab.notice = Some(format!(
                            "Slate couldn't read all of this file ({why}), so line numbers and editing are off."
                        ));
                    }
                    self.apply_goto(i);
                    self.dirty_view = true;
                }
                None => running = true,
            }
        }
        if let Some(st) = self.tabs[i].save.as_mut() {
            match st.job.take() {
                Some(r) => {
                    let st = self.tabs[i].save.take().unwrap();
                    self.finish_save(i, st, r);
                }
                None => running = true,
            }
        }
        if let Some(t) = self.tabs.get_mut(i).and_then(|t| t.task.as_mut()) {
            match t.job.take() {
                Some(r) => {
                    let task = self.tabs[i].task.take().unwrap();
                    self.finish_task(i, task.kind, task.version, r);
                }
                None => running = true,
            }
        }
        running
    }

    /// Tab `i`'s file was read, or couldn't be. Returns false if the tab is gone: its file couldn't be opened, or
    /// another tab has that file open already (under another name: a link, `..`), which is shown instead.
    fn opened(&mut self, i: usize, opened: Opened) -> bool {
        self.dirty_title = true;
        self.dirty_view = true;
        let id = self.tabs[i].id;
        let reading_again = self.tabs[i].reload.is_some();
        if opened.loading.is_ok() {
            let dup = opened.canon.as_ref().and_then(|c| self.tabs.iter().position(|t| t.id != id && t.canon.as_ref() == Some(c)));
            if let Some(k) = dup {
                // (a `--wait` for this tab waits for that one)
                let other = self.tabs[k].id;
                for (_, ids) in self.waits.iter_mut().filter(|(_, ids)| ids.contains(&id)) {
                    ids.retain(|&t| t != id);
                    ids.push(other);
                }
                let goto = self.tabs[i].goto.take().filter(|_| self.tabs[i].doc.path.as_deref() != Some(opened.path.as_path()));
                self.tabs.remove(i);
                let k = if k > i { k - 1 } else { k };
                self.active = k;
                if let Some(g) = goto {
                    self.tabs[k].goto = Some(g);
                    self.goto_now(k);
                }
                return false;
            }
            let tab = &mut self.tabs[i];
            if opened.canon.is_some() {
                tab.canon = opened.canon.clone();
            }
            if opened.read_only && !reading_again {
                tab.read_only = true;
            }
            if tab.doc.path.as_deref() == Some(opened.path.as_path()) {
                // (a file of that very name, colons and all: no line to go to)
                tab.goto = None;
            } else {
                // `notes.txt:120` was `notes.txt`
                tab.doc.path = Some(opened.path.clone());
            }
            if !reading_again {
                self.settings.add_recent(&opened.path);
                super::note_recent(&opened.path);
            }
        }
        match opened.loading {
            Ok(Loading::Ready(doc)) => self.loaded(i, doc, None),
            Ok(Loading::Indexing(doc, job)) => self.loaded(i, doc, Some(job)),
            Ok(Loading::Converting(job)) => self.tabs[i].convert = Some(job),
            Err(_) if opened.creatable => {
                // a new file: saving creates it
                let tab = &mut self.tabs[i];
                tab.doc.path = Some(opened.path.clone());
                tab.lang = detect(Some(&opened.path), &tab.doc);
                let name = tab.title();
                self.flash(format!("{name} is a new file: saving creates it."), false);
            }
            Err(e) => {
                let name = self.tabs[i].doc.path.as_deref().unwrap_or(&opened.path).display().to_string();
                if reading_again {
                    // (the text as it was stays)
                    self.tabs[i].reload = None;
                    self.flash(format!("Couldn't read {name} again: {}", fileio::friendly_io(&e)), true);
                    return true;
                }
                self.flash(format!("Couldn't open {name}: {}", fileio::friendly_io(&e)), true);
                // the tab goes (unless it's the only one); the tab shown stays shown
                if self.tabs.len() > 1 {
                    self.tabs.remove(i);
                    self.keep_shown(i);
                    self.tab_gone(id);
                    return false;
                }
                let tab = &mut self.tabs[i];
                tab.doc = Document::new();
                tab.goto = None;
            }
        }
        if opened.binary {
            self.tabs[i].notice = Some("This looks like a binary file (a program, an image...): shown as text.".into());
        }
        true
    }

    fn loaded(&mut self, i: usize, mut doc: Document, index: Option<Job<bool>>) {
        let active = i == self.active;
        let tab = &mut self.tabs[i];
        if doc.path.is_none() {
            doc.path = tab.doc.path.clone();
        }
        tab.lang = if tab.lang_picked { tab.lang } else { detect(doc.path.as_deref(), &doc) };
        let head = doc.read(0, (64 << 10).min(doc.len()));
        if let Some(ind) = edit::detect_indent(&head) {
            tab.indent = ind;
        }
        tab.doc = doc;
        tab.index = index;
        let old = std::mem::take(&mut tab.view);
        // (the bookmarks stay: on line starts of the text as it is now)
        let mut marks = tab.restore_marks.take().unwrap_or(old.bookmarks.clone());
        edit::tidy_marks(&mut marks, &tab.doc);
        tab.view.bookmarks = marks;
        tab.seen_disk = None;
        let len = tab.doc.len();
        if let Some(follow_end) = tab.reload.take() {
            // read again after another program changed it: the view stays where it was (at the end, if the caret was:
            // a log that grows is followed)
            let v = &mut tab.view;
            v.sel = if follow_end { Sel::at(len) } else { Sel::new(old.sel.anchor.min(len), old.sel.caret.min(len)) };
            v.top = old.top.min(len);
            v.scroll_x = old.scroll_x;
            if follow_end && active {
                self.reveal_pending = true;
            }
        } else if let Some((anchor, caret, top)) = tab.restore_at.take() {
            tab.view.sel = Sel::new(anchor.min(len), caret.min(len));
            tab.view.top = top.min(len);
        }
        self.apply_goto(i);
        self.dirty_title = true;
        self.dirty_view = true;
    }

    // ---- saving ----

    pub fn exec_save(&mut self, i: usize, close_after: bool) {
        match self.tabs[i].doc.path.clone() {
            Some(p) => {
                let enc = self.tabs[i].doc.encoding;
                self.start_save(i, p, enc, close_after, false);
            }
            None => {
                let tab = self.tabs[i].id;
                self.asks.push(Ask::SaveAs { tab, close_after });
            }
        }
    }

    pub fn start_save(&mut self, i: usize, path: PathBuf, encoding: Encoding, close_after: bool, lossy_ok: bool) {
        let notify = self.notify.clone();
        let tab = &mut self.tabs[i];
        if tab.save.is_some() {
            self.flash("Still saving — try again when it's done.", true);
            return;
        }
        if !tab.doc.is_ready() || tab.loading() {
            self.flash("Still opening the file — try saving again in a moment.", true);
            return;
        }
        let snap = tab.doc.snapshot();
        let state = tab.doc.state_id();
        let version = tab.doc.version;
        let bom = tab.doc.bom;
        let p = path.clone();
        let damaged = tab.doc.bad_units > 0 && !lossy_ok;
        let job = Job::spawn(snap.len(), notify, move |ctx| {
            if damaged {
                return Err(SaveError::Lossy);
            }
            fileio::save(&snap, &p, encoding, bom, lossy_ok, ctx)
        });
        let doc = tab.doc.id();
        tab.save = Some(SaveTask { job, state, version, doc, path, encoding, close_after });
        tab.doc.seal();
        self.dirty_view = true;
    }

    fn finish_save(&mut self, i: usize, st: SaveTask, r: Result<Saved, SaveError>) {
        match r {
            Ok(saved) => {
                let tab = &mut self.tabs[i];
                if tab.doc.id() != st.doc {
                    self.flash("Saved", false);
                    if self.quitting && self.tabs.iter().all(|t| t.save.is_none()) {
                        self.asks.push(Ask::Quit);
                    }
                    return;
                }
                let ext = |p: &Path| p.extension().map(|e| e.to_ascii_lowercase());
                let renamed = tab.doc.path.as_deref().map(ext) != Some(ext(&st.path));
                tab.doc.path = Some(st.path.clone());
                // (another open of the file it is now finds this tab; the shortcuts tab saved as a file is that file)
                tab.canon = saved.canon;
                tab.title_override = None;
                tab.doc.encoding = st.encoding;
                tab.doc.disk = saved.disk;
                tab.doc.mark_saved_at(st.state);
                tab.doc.bad_units = 0;
                tab.seen_disk = None;
                if let Some((src, start, nl)) = saved.rebase {
                    if tab.doc.version == st.version {
                        tab.doc.rebase_on(src, start, nl);
                    }
                }
                if renamed && !tab.lang_picked {
                    tab.lang = detect(Some(&st.path), &tab.doc);
                    tab.view.clear_cache();
                }
                tab.notice = None;
                tab.notice_save_as = false;
                let name = tab.title();
                self.settings.add_recent(&st.path);
                super::note_recent(&st.path);
                self.session_dirty = true;
                if saved.lossy && st.encoding == Encoding::Ansi {
                    self.flash("Some characters can't be stored in ANSI and were saved as \"?\".", true);
                } else if saved.lossy {
                    self.flash(format!("Bytes that weren't text were saved in {} as \"\u{FFFD}\".", st.encoding.label()), true);
                } else {
                    self.flash(format!("Saved {name}"), false);
                }
                if st.close_after && !self.tabs[i].doc.is_dirty() {
                    self.remove_tab(i);
                }
                // (closing waited for this save: it carries on)
                if self.quitting && self.tabs.iter().all(|t| t.save.is_none()) {
                    self.asks.push(Ask::Quit);
                }
            }
            Err(SaveError::Lossy) => {
                let tab = self.tabs[i].id;
                self.asks.push(Ask::Lossy { tab, path: st.path, encoding: st.encoding, close_after: st.close_after });
            }
            Err(SaveError::Cancelled) => {
                self.flash("Saving was cancelled.", false);
                self.quitting = false;
            }
            Err(e) => {
                let tab = &mut self.tabs[i];
                tab.notice = Some(format!("Couldn't save {}: {e}", st.path.display()));
                tab.notice_save_as = true;
                self.flash("Couldn't save (see above the text).", true);
                self.quitting = false;
            }
        }
        self.dirty_title = true;
        self.dirty_view = true;
    }

    // ---- tabs ----

    /// Closes tab `i`: right away when it has nothing unsaved (or the session keeps its text: no, a closed tab's
    /// text isn't kept anywhere, so unsaved changes are asked about).
    pub fn close_tab(&mut self, i: usize) {
        if let Some(st) = self.tabs[i].save.as_mut() {
            // (when nothing was typed since, it closes once saved; otherwise closing it again asks)
            st.close_after = true;
            self.flash("Saving… the tab closes once that's done.", false);
            return;
        }
        if self.tabs[i].doc.is_dirty() {
            let tab = self.tabs[i].id;
            self.asks.push(Ask::CloseUnsaved { tab });
            return;
        }
        self.remove_tab(i);
    }

    pub fn remove_tab(&mut self, i: usize) {
        let tab = self.tabs.remove(i);
        if !self.waits.is_empty() {
            self.tab_gone(tab.id);
        }
        if let Some(p) = tab.doc.path.clone() {
            self.closed.retain(|q| q != &p);
            self.closed.push(p);
            if self.closed.len() > 20 {
                self.closed.remove(0);
            }
        }
        if self.tabs.is_empty() {
            self.new_untitled();
        }
        self.keep_shown(i);
        self.session_dirty = true;
        self.dirty_title = true;
        self.dirty_view = true;
    }

    /// Tab `removed` was taken out: the tab shown stays the one shown (the next, if it was that one).
    fn keep_shown(&mut self, removed: usize) {
        if self.active > removed || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    pub fn activate(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            // (a message belongs to the tab it was about: it stood where the caret's place shows)
            self.flash = None;
            self.find.found = None;
            self.dirty_title = true;
            self.dirty_view = true;
        }
    }

    // ---- editing ----

    pub fn editable(&mut self) -> bool {
        let tab = &self.tabs[self.active];
        if tab.filter.is_some() {
            self.flash("These are lines of another tab, to look through: Enter or a double-click goes to one there.", false);
            return false;
        }
        if tab.read_only {
            self.flash("This tab is read only: click “Read only” on the right (or File → Read only) to edit it.", true);
            return false;
        }
        if tab.loading() {
            self.flash("Still opening the file…", false);
            return false;
        }
        if !tab.doc.is_ready() {
            self.flash("Still reading this file's lines — editing is on once that's done.", false);
            return false;
        }
        if tab.task.is_some() {
            self.flash("Still working on this file…", false);
            return false;
        }
        true
    }

    /// After the text changed: the view takes the changes, the caret is shown.
    pub fn after_edit(&mut self, cx_reveal: bool) {
        let tab = &mut self.tabs[self.active];
        tab.view.sync(&mut tab.doc);
        tab.view.want_x = None;
        self.find.found = None;
        self.session_dirty = true;
        self.dirty_title = true;
        self.dirty_view = true;
        self.caret_on = true;
        if cx_reveal {
            self.reveal_pending = true;
        }
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() || !self.editable() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let sel = tab.view.sel;
        let bytes = text.as_bytes();
        let new = if bytes.len() == 1 && matches!(bytes[0], b'}' | b']' | b')') {
            edit::close_bracket(&mut tab.doc, sel, bytes[0])
                .unwrap_or_else(|| edit::replace_selection(&mut tab.doc, sel, bytes, EditKind::Typing))
        } else {
            let fixed = edit::normalize_eols(bytes, tab.doc.eol.as_bytes());
            let kind = if fixed.len() > 1 { EditKind::Other } else { EditKind::Typing };
            edit::replace_selection(&mut tab.doc, sel, &fixed, kind)
        };
        tab.view.sel = new;
        self.after_edit(true);
    }

    pub fn paste_text(&mut self, text: &str) {
        if !self.editable() {
            return;
        }
        let line = self.copied_line.as_deref() == Some(text);
        let tab = &mut self.tabs[self.active];
        let fixed = edit::normalize_eols(text.as_bytes(), tab.doc.eol.as_bytes());
        if line && tab.view.sel.is_empty() {
            // a whole line (copied with nothing selected) goes above the caret's line, the caret staying where it is
            let caret = tab.view.sel.caret;
            let at = tab.doc.line_start_of(caret);
            edit::replace_selection(&mut tab.doc, Sel::at(at), &fixed, EditKind::Other);
            tab.doc.seal();
            tab.view.sel = Sel::at(caret + fixed.len() as u64);
            self.after_edit(true);
            return;
        }
        let new = edit::replace_selection(&mut tab.doc, tab.view.sel, &fixed, EditKind::Other);
        tab.doc.seal();
        tab.view.sel = new;
        self.after_edit(true);
    }

    fn selected_text(&self) -> Option<String> {
        let tab = self.tab();
        let sel = tab.view.sel;
        if sel.is_empty() {
            // the whole line, with its line break
            let a = tab.doc.line_start_of(sel.caret);
            let b = tab.doc.next_newline(a).map_or(tab.doc.len(), |p| p + 1);
            if b - a > edit::COPY_MAX {
                return None;
            }
            return Some(String::from_utf8_lossy(&tab.doc.read(a, b)).into_owned());
        }
        if sel.end() - sel.start() > 256 << 20 {
            return None;
        }
        Some(String::from_utf8_lossy(&tab.doc.read(sel.start(), sel.end())).into_owned())
    }

    /// Runs `f` with the active tab's view and what it needs to lay the text out (`g`: the text area).
    pub fn with_view<R>(&mut self, g: &Geom, f: impl FnOnce(&mut View, &Ctx) -> R) -> R {
        let App { tabs, active, style, theme, .. } = self;
        let tab = &mut tabs[*active];
        let cx = Ctx {
            doc: &tab.doc,
            lang: tab.lang,
            style,
            theme,
            pango: &g.pango,
            width: g.width,
            height: g.height,
        };
        f(&mut tab.view, &cx)
    }

    /// The editing and moving keys (the rest are menu shortcuts); `g` is the text area. Returns whether it was one
    /// of them.
    pub fn on_key(&mut self, g: &Geom, key: gdk::Key, mods: gdk::ModifierType) -> bool {
        use gdk::Key;
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        self.caret_on = true;
        let moving = |app: &mut App, f: &mut dyn FnMut(&mut View, &Ctx)| {
            app.with_view(g, |v, c| f(v, c));
            app.reveal_pending = true;
            app.dirty_view = true;
        };
        match key {
            Key::Left | Key::KP_Left if !alt => {
                moving(self, &mut |v, c| {
                    let pos = if !shift && !v.sel.is_empty() {
                        v.sel.start()
                    } else if ctrl {
                        c.doc.word_left(v.sel.caret)
                    } else {
                        edit::prev_cluster(c.doc, v.sel.caret)
                    };
                    v.set_caret(pos, shift);
                    v.want_x = None;
                });
            }
            Key::Right | Key::KP_Right if !alt => {
                moving(self, &mut |v, c| {
                    let pos = if !shift && !v.sel.is_empty() {
                        v.sel.end()
                    } else if ctrl {
                        c.doc.word_right(v.sel.caret)
                    } else {
                        edit::next_cluster(c.doc, v.sel.caret)
                    };
                    v.set_caret(pos, shift);
                    v.want_x = None;
                });
            }
            Key::Up | Key::KP_Up if alt => self.exec(Cmd::MoveLineUp),
            Key::Down | Key::KP_Down if alt => self.exec(Cmd::MoveLineDown),
            Key::Up | Key::KP_Up if ctrl => moving(self, &mut |v, c| v.scroll_rows(c, -1)),
            Key::Down | Key::KP_Down if ctrl => moving(self, &mut |v, c| v.scroll_rows(c, 1)),
            Key::Up | Key::KP_Up => moving(self, &mut |v, c| v.move_rows(c, -1, shift)),
            Key::Down | Key::KP_Down => moving(self, &mut |v, c| v.move_rows(c, 1, shift)),
            // (Ctrl+PgUp / PgDn switch tabs: the menu's shortcuts)
            Key::Page_Up | Key::KP_Page_Up if !ctrl => moving(self, &mut |v, c| v.page(c, false, shift)),
            Key::Page_Down | Key::KP_Page_Down if !ctrl => moving(self, &mut |v, c| v.page(c, true, shift)),
            Key::Home | Key::KP_Home if ctrl => moving(self, &mut |v, _| {
                v.set_caret(0, shift);
                v.want_x = None;
            }),
            Key::End | Key::KP_End if ctrl => moving(self, &mut |v, c| {
                v.set_caret(c.doc.len(), shift);
                v.want_x = None;
            }),
            Key::Home | Key::KP_Home => moving(self, &mut |v, c| {
                v.home(c, shift);
                v.want_x = None;
            }),
            Key::End | Key::KP_End => moving(self, &mut |v, c| {
                v.end(c, shift);
                v.want_x = None;
            }),
            Key::BackSpace => {
                if self.editable() {
                    let tab = &mut self.tabs[self.active];
                    tab.view.sel = edit::backspace(&mut tab.doc, tab.view.sel, ctrl);
                    self.after_edit(true);
                }
            }
            Key::Delete | Key::KP_Delete if shift && !ctrl => self.exec(Cmd::Cut),
            Key::Delete | Key::KP_Delete => {
                if self.editable() {
                    let tab = &mut self.tabs[self.active];
                    tab.view.sel = edit::delete_forward(&mut tab.doc, tab.view.sel, ctrl);
                    self.after_edit(true);
                }
            }
            Key::Return | Key::KP_Enter if !ctrl && !alt && self.tab().filter.is_some() => self.go_to_source(),
            Key::Return | Key::KP_Enter if !ctrl && !alt => {
                if self.editable() {
                    let unit = self.tabs[self.active].indent.unit();
                    let tab = &mut self.tabs[self.active];
                    tab.view.sel = edit::newline(&mut tab.doc, tab.view.sel, &unit);
                    self.after_edit(true);
                }
            }
            Key::Tab | Key::ISO_Left_Tab if !ctrl && !alt => {
                if self.editable() {
                    let (ind, ts) = (self.tabs[self.active].indent, self.settings.tab_size);
                    let tab = &mut self.tabs[self.active];
                    let sel = tab.view.sel;
                    let multi = !sel.is_empty() && tab.doc.line_start_of(sel.start()) != tab.doc.line_start_of(sel.end());
                    let outdent = shift || key == Key::ISO_Left_Tab;
                    if multi || outdent {
                        match edit::indent_lines(&mut tab.doc, sel, ind, ts, outdent) {
                            Ok(s) => tab.view.sel = s,
                            Err(e) => {
                                self.flash(e, true);
                                return true;
                            }
                        }
                    } else {
                        let text = edit::tab_text(&tab.doc, sel.start(), ind, ts);
                        tab.view.sel = edit::replace_selection(&mut tab.doc, sel, &text, EditKind::Typing);
                    }
                    self.after_edit(true);
                }
            }
            Key::Escape => {
                if self.find.open {
                    self.exec(Cmd::CloseFind);
                } else if let Some(t) = self.tabs[self.active].task.as_ref() {
                    t.job.cancel();
                    self.flash("Cancelling…", false);
                } else if !self.tabs[self.active].view.sel.is_empty() {
                    let c = self.tabs[self.active].view.sel.caret;
                    self.tabs[self.active].view.sel = Sel::at(c);
                    self.dirty_view = true;
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    // ---- commands ----

    pub fn exec(&mut self, cmd: Cmd) {
        self.caret_on = true;
        self.dirty_view = true;
        match cmd {
            Cmd::NewTab => {
                self.new_untitled();
            }
            Cmd::Open => self.asks.push(Ask::Open),
            Cmd::Save => self.exec_save(self.active, false),
            Cmd::SaveAs => {
                let tab = self.tab().id;
                self.asks.push(Ask::SaveAs { tab, close_after: false });
            }
            Cmd::SaveAll => {
                for i in 0..self.tabs.len() {
                    if self.tabs[i].doc.is_dirty() {
                        self.exec_save(i, false);
                    }
                }
            }
            Cmd::CloseTab => self.close_tab(self.active),
            Cmd::ReopenClosed => {
                if let Some(p) = self.closed.pop() {
                    self.open_paths(&[p]);
                }
            }
            Cmd::Quit => self.asks.push(Ask::Quit),
            Cmd::Undo | Cmd::Redo => {
                if !self.editable() {
                    return;
                }
                let tab = &mut self.tabs[self.active];
                let r = if cmd == Cmd::Undo { tab.doc.undo() } else { tab.doc.redo() };
                match r {
                    Some(sel) => {
                        tab.view.sel = sel;
                        self.after_edit(true);
                        self.reveal_center = true;
                    }
                    None => self.flash(if cmd == Cmd::Undo { "Nothing to undo" } else { "Nothing to redo" }, false),
                }
            }
            Cmd::Copy => match self.selected_text() {
                Some(t) => {
                    self.copied_line = self.tab().view.sel.is_empty().then(|| t.clone());
                    self.copy_out = Some(t);
                }
                None => self.flash("That's too much text to copy.", true),
            },
            Cmd::Cut => {
                if !self.editable() {
                    return;
                }
                let Some(t) = self.selected_text() else {
                    self.flash("That's too much text to cut.", true);
                    return;
                };
                self.copied_line = self.tab().view.sel.is_empty().then(|| t.clone());
                self.copy_out = Some(t);
                let tab = &mut self.tabs[self.active];
                let sel = tab.view.sel;
                let sel = if sel.is_empty() {
                    let a = tab.doc.line_start_of(sel.caret);
                    let b = tab.doc.next_newline(a).map_or(tab.doc.len(), |p| p + 1);
                    Sel::new(a, b)
                } else {
                    sel
                };
                tab.view.sel = edit::replace_selection(&mut tab.doc, sel, b"", EditKind::Other);
                self.after_edit(true);
            }
            Cmd::Paste => self.paste_wanted = true,
            Cmd::SelectAll => {
                let tab = &mut self.tabs[self.active];
                tab.view.sel = Sel::new(0, tab.doc.len());
            }
            Cmd::Find | Cmd::Replace => {
                self.find.open = true;
                self.find.replace = cmd == Cmd::Replace;
                // the selection (one line of it) is what to find
                let tab = self.tab();
                let sel = tab.view.sel;
                self.find.sel_before = (!sel.is_empty()).then(|| (sel.start(), sel.end()));
                if !sel.is_empty() && sel.end() - sel.start() < 512 {
                    let t = tab.doc.read(sel.start(), sel.end());
                    if !t.contains(&b'\n') {
                        self.find.query.text = String::from_utf8_lossy(&t).into_owned();
                        self.query_changed();
                    }
                }
            }
            Cmd::CloseFind => {
                self.find.open = false;
                self.find.found = None;
                self.find.history_at = None;
                edit::remember(&mut self.settings.find_history, &self.find.query.text.clone());
                self.tabs[self.active].view.scope = None;
            }
            Cmd::FindNext | Cmd::FindPrev => self.find_step(cmd == Cmd::FindNext),
            Cmd::ReplaceOne => self.replace_one(),
            Cmd::ReplaceAll => self.start_task(TaskKind::ReplaceAll),
            Cmd::GotoLine => self.asks.push(Ask::GotoLine),
            Cmd::Print => self.print(),
            Cmd::ToggleComment => {
                let tab = self.tab();
                let Some(style) = edit::comment_style(&tab.doc, tab.lang, tab.view.sel.start()) else {
                    self.flash(format!("{} has no comments", self.tab().lang.label()), true);
                    return;
                };
                if !self.editable() {
                    return;
                }
                let tab = &mut self.tabs[self.active];
                match edit::toggle_comment(&mut tab.doc, tab.view.sel, style) {
                    Ok(s) => {
                        tab.view.sel = s;
                        self.after_edit(true);
                    }
                    Err(e) => self.flash(e, true),
                }
            }
            Cmd::DuplicateLine | Cmd::MoveLineUp | Cmd::MoveLineDown => {
                if !self.editable() {
                    return;
                }
                let tab = &mut self.tabs[self.active];
                let r = match cmd {
                    Cmd::DuplicateLine => edit::duplicate(&mut tab.doc, tab.view.sel),
                    _ => edit::move_lines(&mut tab.doc, tab.view.sel, cmd == Cmd::MoveLineDown),
                };
                match r {
                    Ok(s) => {
                        tab.view.sel = s;
                        self.after_edit(true);
                    }
                    Err(e) => self.flash(e, true),
                }
            }
            Cmd::DeleteLine => {
                if !self.editable() {
                    return;
                }
                let tab = &mut self.tabs[self.active];
                if let Some(s) = edit::delete_lines(&mut tab.doc, tab.view.sel) {
                    tab.view.sel = s;
                    self.after_edit(true);
                }
            }
            Cmd::Lines(op) => self.lines_op(op),
            Cmd::Case(op) => self.case_op(op),
            Cmd::JsonFormat => self.start_task(TaskKind::Json(json::Mode::Pretty)),
            Cmd::JsonMinify => self.start_task(TaskKind::Json(json::Mode::Minify)),
            Cmd::JsonCheck => self.start_task(TaskKind::Json(json::Mode::Validate)),
            Cmd::CharInfo => {
                let tab = self.tab();
                match edit::char_info(&tab.doc, tab.view.sel.caret, tab.doc.encoding) {
                    Some(s) => self.flash(s, false),
                    None => self.flash("The caret is at the end of the text: no character there.", false),
                }
            }
            Cmd::CsvCheck => match edit::csv_dialect(self.tab().lang, &self.tab().doc) {
                Some(d) => self.start_task(TaskKind::CsvCheck(d)),
                None => self.flash("Check works for JSON, XML, CSV and TSV files (the language is in the status bar).", true),
            },
            Cmd::CsvSort(desc) => {
                let tab = self.tab();
                let Some(d) = edit::csv_dialect(tab.lang, &tab.doc) else {
                    self.flash("Sort by this column works in CSV and TSV files (the language is in the status bar).", true);
                    return;
                };
                if !tab.doc.is_ready() {
                    self.flash("Still reading the file's lines — try again in a moment.", true);
                    return;
                }
                let Some((col, _)) = edit::csv_field(&tab.doc, d, tab.view.sel.caret) else {
                    self.flash("Put the caret in the column to sort by (nearer its line's start).", true);
                    return;
                };
                let (_, header) = edit::csv_header(&tab.doc, d);
                self.start_task(TaskKind::SortColumn { col, desc, d, header });
            }
            Cmd::XmlFormat => self.start_task(TaskKind::Xml(json::Mode::Pretty)),
            Cmd::XmlMinify => self.start_task(TaskKind::Xml(json::Mode::Minify)),
            Cmd::Format => match self.tab().lang {
                Lang::Json => self.start_task(TaskKind::Json(json::Mode::Pretty)),
                Lang::Xml => self.start_task(TaskKind::Xml(json::Mode::Pretty)),
                _ => self.flash("Formatting works for JSON and XML files (the language is in the status bar).", true),
            },
            Cmd::XmlCheck => self.start_task(TaskKind::Xml(json::Mode::Validate)),
            Cmd::Wrap => {
                self.settings.wrap = !self.settings.wrap;
                self.style.wrap = self.settings.wrap;
                for t in &mut self.tabs {
                    t.view.scroll_x = 0.0;
                    t.view.top_row = 0;
                }
            }
            Cmd::LineNumbers => {
                self.settings.line_numbers = !self.settings.line_numbers;
                self.style.line_numbers = self.settings.line_numbers;
            }
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                let z = self.settings.zoom;
                let z = match cmd {
                    Cmd::ZoomIn => ZOOM_STEPS.iter().copied().find(|&s| s > z + 0.001).unwrap_or(5.0),
                    Cmd::ZoomOut => ZOOM_STEPS.iter().rev().copied().find(|&s| s < z - 0.001).unwrap_or(0.5),
                    _ => 1.0,
                };
                self.settings.zoom = z;
                self.restyle = true;
                self.flash(format!("Zoom {}%", (z * 100.0).round()), false);
            }
            Cmd::Theme(m) => {
                self.settings.theme = m;
                self.apply_theme();
            }
            Cmd::NextTab | Cmd::PrevTab => {
                let n = self.tabs.len();
                let i = if cmd == Cmd::NextTab { (self.active + 1) % n } else { (self.active + n - 1) % n };
                self.activate(i);
            }
            Cmd::GoTab(k) => {
                let i = if k >= 9 { self.tabs.len() - 1 } else { k.saturating_sub(1) };
                self.activate(i.min(self.tabs.len() - 1));
            }
            Cmd::SetEol(e) => {
                // every line break in the text (as on Windows), and the ones typed from now on
                let tab = &mut self.tabs[self.active];
                if tab.doc.is_empty() {
                    tab.doc.eol = e;
                } else {
                    self.start_task(TaskKind::Eol(e));
                }
            }
            Cmd::SetLang(l) => {
                let tab = &mut self.tabs[self.active];
                tab.lang = l;
                tab.lang_picked = true;
                tab.view.clear_cache();
            }
            Cmd::InsertDateTime => self.insert_text(&now_text()),
            Cmd::About => self.asks.push(Ask::About),
            Cmd::Shortcuts => self.asks.push(Ask::Shortcuts),
            Cmd::MakeDefault => self.asks.push(Ask::MakeDefault),
            Cmd::StopDefault => self.asks.push(Ask::StopDefault),
            Cmd::ToggleReadOnly => {
                let tab = &mut self.tabs[self.active];
                if tab.filter.is_some() {
                    self.flash("A tab of matching lines stays read only.", false);
                } else {
                    tab.read_only = !tab.read_only;
                    let on = tab.read_only;
                    self.session_dirty = true;
                    self.flash(if on { "Read only: the text can't be changed (File → Read only)" } else { "Editable" }, false);
                }
            }
            Cmd::FilterLines(invert) => self.filter_lines(invert),
            Cmd::ToggleBookmark => {
                let tab = &mut self.tabs[self.active];
                let caret = tab.view.sel.caret;
                edit::toggle_mark(&mut tab.view.bookmarks, &tab.doc, caret);
                self.session_dirty = true;
            }
            Cmd::NextBookmark | Cmd::PrevBookmark => {
                let tab = &mut self.tabs[self.active];
                match edit::next_mark(&tab.view.bookmarks, &tab.doc, tab.view.sel.caret, cmd == Cmd::NextBookmark) {
                    Some(p) => {
                        tab.view.sel = Sel::at(p);
                        tab.view.want_x = None;
                        self.reveal_pending = true;
                        self.reveal_center = true;
                    }
                    None => self.flash("No bookmarks in this tab: Ctrl+F2 bookmarks the caret's line.", false),
                }
            }
            Cmd::ClearBookmarks => {
                self.tabs[self.active].view.bookmarks.clear();
                self.session_dirty = true;
            }
        }
        self.dirty_title = true;
    }

    fn lines_op(&mut self, op: lines::LineOp) {
        if !self.editable() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let sel = tab.view.sel;
        if sel.is_empty() || (sel.start() == 0 && sel.end() == tab.doc.len()) {
            self.start_task(TaskKind::Lines(op));
            return;
        }
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
            self.flash(edit::nothing_to_clean(op), false);
            return;
        }
        tab.view.sel = edit::replace_range(&mut tab.doc, sel, a, b, &out);
        self.after_edit(true);
        self.flash(edit::lines_done(op, count), false);
    }

    fn case_op(&mut self, op: lines::CaseOp) {
        if !self.editable() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let mut sel = tab.view.sel;
        if sel.is_empty() {
            let (wa, wb) = tab.doc.word_at(sel.caret);
            if wa == wb {
                return;
            }
            sel = Sel::new(wa, wb);
        }
        if sel.end() - sel.start() > SELECTION_MAX {
            self.flash("Select less text (up to 16 MB).", true);
            return;
        }
        let text = tab.doc.read(sel.start(), sel.end());
        let out = lines::change_case(&text, op);
        let before = tab.view.sel;
        let new = edit::replace_range(&mut tab.doc, before, sel.start(), sel.end(), &out);
        tab.view.sel = Sel::new(sel.start(), new.caret.max(sel.start()));
        self.after_edit(true);
    }

    fn start_task(&mut self, kind: TaskKind) {
        let notify = self.notify.clone();
        let changes = !matches!(
            kind,
            TaskKind::Json(json::Mode::Validate) | TaskKind::Xml(json::Mode::Validate) | TaskKind::Filter(_) | TaskKind::CsvCheck(_)
        );
        if changes && !self.editable() {
            return;
        }
        let indent = vec![b' '; self.settings.json_indent as usize];
        let query = self.find.query.text.clone();
        let matcher = match kind {
            TaskKind::ReplaceAll | TaskKind::Filter(_) => match self.find.matcher.clone() {
                Some(m) => Some(m),
                None => {
                    self.flash("Type something to find first", true);
                    return;
                }
            },
            _ => None,
        };
        let replacement = edit::replacement(&self.find.replacement, self.find.query.regex, self.tab().doc.eol.as_bytes());
        if kind == TaskKind::ReplaceAll {
            let typed = self.find.replacement.clone();
            edit::remember(&mut self.settings.replace_history, &typed);
            edit::remember(&mut self.settings.find_history, &self.find.query.text.clone());
        }
        let scope = self.tab().view.scope;
        let tab = &mut self.tabs[self.active];
        if tab.task.is_some() {
            self.flash("Still working on the previous task (Esc cancels it).", true);
            return;
        }
        let snap = tab.doc.snapshot();
        let version = tab.doc.version;
        let eol = tab.doc.eol.as_bytes().to_vec();
        let len = snap.len();
        if let TaskKind::Lines(_) | TaskKind::SortColumn { .. } = kind {
            if len > LINES_MAX || tab.doc.line_count().unwrap_or(0) > lines::COUNT_MAX {
                self.flash("Sorting and cleaning up lines works for files up to 512 MB and 32 million lines.", true);
                return;
            }
        }
        let day_first = crate::core::os::day_first();
        let job = Job::spawn(len, notify, move |ctx| {
            guarded(&snap, || match kind {
                TaskKind::Json(mode) | TaskKind::Xml(mode) if mode != json::Mode::Validate => {
                    let hint = if mode == json::Mode::Pretty { len.saturating_mul(3) } else { len };
                    let mut sink = match Sink::limited(hint, edit::transform_max(len)) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let (w, ix) = (Some(&mut sink as &mut dyn std::io::Write), Some(&mut idx));
                    let r = if matches!(kind, TaskKind::Json(_)) {
                        json::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg))
                    } else {
                        xml::run(&snap, mode, &indent, &eol, w, ix, ctx).map(|_| ()).map_err(|e| (e.offset, e.msg))
                    };
                    match r {
                        Ok(()) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Content { src, nl, count: 0 },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        // (the result, not the text: a full disk, or far too big)
                        Err((_, msg)) if msg.starts_with(json::WRITE_FAILED) => {
                            match edit::format_stopped(matches!(kind, TaskKind::Xml(_)), &snap, msg, ctx) {
                                Ok(msg) => TaskResult::Failed(msg),
                                Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                            }
                        }
                        Err((offset, msg)) => TaskResult::FormatError { offset, msg },
                    }
                }
                TaskKind::Json(_) => match json::run(&snap, json::Mode::Validate, b"", b"\n", None, None, ctx) {
                    Ok(s) if s.values > 1 => TaskResult::Valid(format!("Valid JSON Lines: {} values", s.values)),
                    Ok(s) => TaskResult::Valid(format!("Valid JSON (nesting depth {})", s.max_depth)),
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err(e) => TaskResult::FormatError { offset: e.offset, msg: e.msg },
                },
                TaskKind::Xml(_) => match xml::run(&snap, json::Mode::Validate, b"", b"\n", None, None, ctx) {
                    Ok(s) => TaskResult::Valid(format!("Valid XML: {} elements (nesting depth {})", s.elements, s.max_depth)),
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err(e) => TaskResult::FormatError { offset: e.offset, msg: e.msg },
                },
                TaskKind::CsvCheck(d) => match csv::check(&snap, d, ctx) {
                    Ok(s) => TaskResult::Valid(edit::csv_valid(d, s)),
                    Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                    Err(e) => TaskResult::FormatError { offset: e.offset, msg: e.msg },
                },
                TaskKind::SortColumn { col, desc, d, header } => {
                    let mut buf = Vec::new();
                    let text = snap.slice(0, snap.len(), &mut buf);
                    let mut sink = match Sink::new(text.len() as u64) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let sorted = match csv::sort_by_column(text, d, col, desc, day_first, header, &mut |piece| {
                        idx.push(piece);
                        std::io::Write::write_all(&mut sink, piece)
                    }) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    };
                    drop(buf);
                    match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count: sorted.rows },
                        Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    }
                }
                TaskKind::Lines(op) => {
                    let mut buf = Vec::new();
                    let text = snap.slice(0, snap.len(), &mut buf);
                    let mut sink = match Sink::new(text.len() as u64) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    let count = match lines::apply_to(op, text, &mut |piece| {
                        idx.push(piece);
                        std::io::Write::write_all(&mut sink, piece)
                    }) {
                        Ok(count) => count,
                        Err(e) => return TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    };
                    drop(buf);
                    match sink.finish(idx) {
                        Ok((src, nl)) => TaskResult::Content { src, nl, count },
                        Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                    }
                }
                TaskKind::ReplaceAll => {
                    let m = matcher.unwrap();
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
                }
                TaskKind::Filter(invert) => {
                    let m = matcher.unwrap();
                    let mut sink = match Sink::new(len) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    match m.filter_lines_to(&snap, invert, &mut sink, &mut idx, ctx) {
                        Ok(found) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Filtered { src, nl, found, query },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the lines: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        Err(e) => TaskResult::Failed(format!("Couldn't go through the lines: {e}")),
                    }
                }
                TaskKind::Eol(e) => {
                    let mut sink = match Sink::new(len + len / 16) {
                        Ok(s) => s,
                        Err(e) => return TaskResult::Failed(format!("Couldn't create a temporary file: {e}")),
                    };
                    let mut idx = IndexBuilder::new();
                    match edit::convert_eol(&snap, e == Eol::Crlf, &mut sink, &mut idx, ctx) {
                        Ok(count) => match sink.finish(idx) {
                            Ok((src, nl)) => TaskResult::Content { src, nl, count },
                            Err(e) => TaskResult::Failed(format!("Couldn't write the result: {e}")),
                        },
                        Err(_) if ctx.cancelled() => TaskResult::Cancelled,
                        Err(e) => TaskResult::Failed(format!("Couldn't convert: {e}")),
                    }
                }
            })
        });
        tab.task = Some(Task { kind, job, version });
        // (the status bar says "Working…" while the task runs: `status_message`; not after it)
        self.flash = None;
        self.dirty_view = true;
    }

    /// A check or format of tab `i` went through: the note about an error an earlier one found goes.
    fn clear_check_error(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        if tab.notice.as_deref().is_some_and(|n| n.starts_with("Not valid ") || n.starts_with("Couldn't format: not valid ")) {
            tab.notice = None;
        }
    }

    fn finish_task(&mut self, i: usize, kind: TaskKind, version: u64, r: TaskResult) {
        match r {
            TaskResult::Content { src, nl, count } => {
                let tab = &mut self.tabs[i];
                if tab.doc.version != version {
                    self.flash("The text changed while working, so nothing was changed. Try again.", true);
                    return;
                }
                if kind == TaskKind::ReplaceAll && count == 0 {
                    self.flash("No matches to replace", true);
                    return;
                }
                if let TaskKind::Lines(op) = kind {
                    if count == 0 {
                        self.flash(edit::nothing_to_clean(op), false);
                        return;
                    }
                }
                if matches!(kind, TaskKind::SortColumn { .. }) && count == 0 {
                    self.flash("The rows are already in that order", false);
                    return;
                }
                self.session_dirty = true;
                self.dirty_title = true;
                if let TaskKind::Eol(e) = kind {
                    tab.doc.eol = e;
                    if count == 0 {
                        // (only the kind typed from now on changed)
                        tab.doc.mark_dirty();
                        self.flash(format!("Line endings are {}", e.short()), false);
                        return;
                    }
                }
                let sel = tab.view.sel;
                let new_len = src.len();
                let places = matches!(kind, TaskKind::Eol(_)).then(|| (tab.doc.place_of(sel.caret), tab.doc.place_of(tab.view.top)));
                // (bookmarks stay on their lines where the lines stay what they were; elsewhere they go)
                let marks =
                    matches!(kind, TaskKind::Eol(_) | TaskKind::ReplaceAll).then(|| edit::marks_as_lines(&tab.view.bookmarks, &tab.doc));
                let (scope, old_len) = (tab.view.scope, tab.doc.len());
                tab.doc.begin(EditKind::Other, sel);
                tab.doc.replace_all_with(src, nl);
                let keep = matches!(kind, TaskKind::ReplaceAll | TaskKind::Lines(_) | TaskKind::Eol(_) | TaskKind::SortColumn { .. });
                let new_sel = match places {
                    Some((caret, _)) => Sel::at(caret.and_then(|p| tab.doc.offset_of_place(p)).unwrap_or(0)),
                    None if keep => Sel::at(sel.caret.min(new_len)),
                    None => Sel::at(0),
                };
                tab.doc.end(new_sel);
                tab.doc.seal();
                tab.view.sel = new_sel;
                tab.view.sync(&mut tab.doc);
                // (Find in selection: Replace all's range ends where its replacements end; anything else ends it)
                tab.view.scope = match kind {
                    TaskKind::ReplaceAll => scope.map(|(a, b)| (a, (b + new_len).saturating_sub(old_len).max(a))),
                    _ => None,
                };
                tab.view.bookmarks = marks.map_or_else(Vec::new, |l| edit::marks_at_lines(&l, &tab.doc));
                if let Some((_, top)) = places {
                    tab.view.top = top.and_then(|p| tab.doc.offset_of_place(p)).unwrap_or(0);
                }
                if !keep {
                    tab.view.top = 0;
                    tab.view.top_row = 0;
                }
                let msg = match kind {
                    TaskKind::ReplaceAll => format!("Replaced {}", edit::plural(count, "match", "matches")),
                    TaskKind::Lines(op) => edit::lines_done(op, count),
                    TaskKind::SortColumn { col, d, header, .. } => {
                        let name = header.then(|| edit::csv_column_name(&edit::csv_header(&tab.doc, d).0, d, col)).flatten();
                        edit::csv_sorted(count, col, name.as_deref(), header)
                    }
                    TaskKind::Eol(e) => format!("Line endings changed to {} ({})", e.short(), edit::plural(count, "line", "lines")),
                    TaskKind::Json(json::Mode::Minify) | TaskKind::Xml(json::Mode::Minify) => "Minified".into(),
                    _ => "Formatted".into(),
                };
                if self.active == i {
                    self.after_edit(false);
                }
                if matches!(kind, TaskKind::Json(_) | TaskKind::Xml(_)) {
                    self.clear_check_error(i);
                }
                self.flash(msg, false);
            }
            TaskResult::Filtered { src, nl, found, query } => {
                let TaskKind::Filter(invert) = kind else { return };
                self.show_filtered(i, invert, src, nl, found, &query);
            }
            TaskResult::Valid(msg) => {
                self.clear_check_error(i);
                self.flash(msg, false);
            }
            TaskResult::FormatError { offset, msg } => {
                let tab = &mut self.tabs[i];
                let line = tab.doc.line_of(offset).map(|l| l + 1);
                // in characters, like the status bar (bytes for a line too long to count)
                let off = offset.min(tab.doc.len());
                let ls = tab.doc.line_start_of(off);
                let col = if off - ls <= 4 << 20 { bytecount::num_chars(&tab.doc.read(ls, off)) as u64 + 1 } else { off - ls + 1 };
                tab.view.sel = Sel::at(offset.min(tab.doc.len()));
                self.reveal_pending = true;
                self.reveal_center = true;
                let at = line.map_or(format!("byte {}", edit::group(offset + 1)), |l| format!("line {}, column {}", edit::group(l), edit::group(col)));
                // Above the text, as on Windows (a message in the status bar is gone after a few seconds); the caret
                // is where it is.
                let (what, check) = match kind {
                    TaskKind::Xml(m) => ("XML", m == json::Mode::Validate),
                    TaskKind::Json(m) => ("JSON", m == json::Mode::Validate),
                    TaskKind::CsvCheck(d) => (d.name(), true),
                    _ => ("JSON", false),
                };
                // (nested deeper than Slate follows isn't invalid)
                let deep = msg == json::TOO_DEEP;
                let verb = match (check, deep) {
                    (true, true) => format!("Couldn't check this {what}"),
                    (true, false) => format!("Not valid {what}"),
                    (false, true) => format!("Couldn't format this {what}"),
                    (false, false) => format!("Couldn't format: not valid {what}"),
                };
                tab.notice = Some(format!("{verb}: {msg} ({at})"));
                tab.notice_save_as = false;
            }
            TaskResult::Failed(msg) => self.flash(msg, true),
            TaskResult::Cancelled => self.flash("Cancelled", false),
        }
        self.dirty_view = true;
    }

    // ---- find ----

    /// The query changed (typed in the find box, or an option): the matcher again, and a count of the matches.
    /// Where the find bar searches in the tab shown: its Find in selection range, else the whole text.
    fn search_range(&self) -> (u64, u64) {
        let tab = self.tab();
        let len = tab.doc.len();
        tab.view.scope.map_or((0, len), |(a, b)| (a.min(len), b.min(len).max(a.min(len))))
    }

    /// Find in selection on or off (Alt+S): on, the find bar searches what's selected until it's closed.
    pub fn toggle_in_sel(&mut self) {
        let (before, last) = (self.find.sel_before, self.find.last_match);
        let tab = &mut self.tabs[self.active];
        if tab.view.scope.take().is_none() {
            let sel = tab.view.sel;
            let selected = (!sel.is_empty()).then(|| (sel.start(), sel.end()));
            // (the match searching as you type selected isn't what the user selected)
            let range = if selected.is_some() && selected != last { selected } else { before.or(selected) };
            let Some((a, b)) = range.filter(|r| r.1 <= tab.doc.len()) else {
                self.flash("Select the text to search in first.", true);
                self.dirty_view = true;
                return;
            };
            tab.view.scope = Some((a, b));
            tab.view.sel = Sel::at(a);
        }
        self.query_changed();
        self.find_as_you_type();
    }

    /// Up and Down in a find bar box: the text the box gets (the searches or replacements before, newest first; past
    /// the newest, empty), and where that is in the history (set once the box has it: `Find::history_at`).
    pub fn history_step(&mut self, find: bool, older: bool) -> Option<(String, Option<usize>)> {
        let hist = if find { &self.settings.find_history } else { &self.settings.replace_history };
        let typed = if find { self.find.query.text.clone() } else { self.find.replacement.clone() };
        let at = match (self.find.history_at, older) {
            // (one the box has already is skipped)
            (None, true) => (0..hist.len()).find(|&i| hist[i] != typed),
            (None, false) => return None,
            (Some(i), true) => Some((i + 1).min(hist.len().saturating_sub(1))),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        if at.is_none() && self.find.history_at.is_none() {
            return None;
        }
        let text = match at {
            Some(i) => hist.get(i).cloned().unwrap_or_default(),
            None => self.find.history_typed.clone(),
        };
        if self.find.history_at.is_none() {
            self.find.history_typed = typed;
        }
        Some((text, at))
    }

    pub fn query_changed(&mut self) {
        self.find.found = None;
        self.find.count_job = None;
        match Matcher::new(&self.find.query) {
            Ok(m) => {
                self.find.matcher = Some(m);
                self.find.error = None;
                self.start_count();
            }
            Err(e) => {
                self.find.matcher = None;
                self.find.error = (!self.find.query.text.is_empty()).then_some(e);
            }
        }
        self.dirty_view = true;
    }

    fn start_count(&mut self) {
        let Some(m) = self.find.matcher.clone() else { return };
        let notify = self.notify.clone();
        let (a, b) = self.search_range();
        let tab = &mut self.tabs[self.active];
        let snap: Snapshot = tab.doc.snapshot();
        let version = tab.doc.version;
        self.find.count_tab = tab.id;
        self.find.count_job = Some((version, Job::spawn(snap.len(), notify, move |ctx| search::count_in(&m, &snap, a, b, ctx))));
    }

    /// While the find bar is open, the matches are counted (and marked) in the tab shown: again in another tab, or
    /// after an edit (in a document up to 16 MiB: a bigger one is counted again when the search changes).
    pub fn keep_count(&mut self) {
        if !self.find.open || self.find.matcher.is_none() {
            return;
        }
        let tab = self.tab();
        let v = tab.doc.version;
        let counted = matches!(&self.find.found, Some((fv, _)) if *fv == v);
        let counting = matches!(&self.find.count_job, Some((cv, _)) if *cv == v);
        if !counted && !counting && (tab.doc.len() <= 16 << 20 || self.find.count_tab != tab.id) {
            self.find.found = None;
            self.start_count();
        }
    }

    fn poll_find(&mut self) -> bool {
        let mut running = false;
        if let Some((v, job)) = self.find.count_job.as_mut() {
            match job.take() {
                Some(found) => {
                    let v = *v;
                    self.find.count_job = None;
                    if self.tabs[self.active].doc.version == v {
                        self.find.found = Some((v, found));
                        self.dirty_view = true;
                    }
                }
                None => running = true,
            }
        }
        if let Some((v, fwd, sel, job)) = self.find.find_job.as_mut() {
            match job.take() {
                Some(hit) => {
                    let (v, fwd, sel) = (*v, *fwd, *sel);
                    self.find.find_job = None;
                    let tab = &mut self.tabs[self.active];
                    // (dropped if the text or the selection changed meanwhile)
                    if tab.doc.version == v && tab.view.sel == sel {
                        self.found(hit, fwd);
                    }
                }
                None => running = true,
            }
        }
        running
    }

    pub fn find_running(&self) -> bool {
        self.find.find_job.is_some() || self.find.count_job.is_some()
    }

    fn find_step(&mut self, fwd: bool) {
        if self.find.matcher.is_none() {
            if !self.find.open {
                self.exec(Cmd::Find);
            }
            return;
        }
        edit::remember(&mut self.settings.find_history, &self.find.query.text);
        let (a, b) = self.search_range();
        let tab = &self.tabs[self.active];
        let sel = tab.view.sel;
        let len = tab.doc.len();
        let (end, start) = (sel.end().clamp(a, b), sel.start().clamp(a, b));
        let m = self.find.matcher.as_ref().unwrap();
        let inside = move |r: Option<(u64, u64)>| r.filter(|x| x.1 <= b);
        if len <= m.sync_limit() {
            let hit = if fwd {
                inside(m.find_fwd(&tab.doc, end, b, None)).or_else(|| inside(m.find_fwd(&tab.doc, a, end, None)))
            } else {
                m.find_back(&tab.doc, a, start, None).or_else(|| inside(m.find_back(&tab.doc, start, b, None)))
            };
            self.found(hit, fwd);
            return;
        }
        let Ok(m) = Matcher::new(&self.find.query) else { return };
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let snap = tab.doc.snapshot();
        let version = tab.doc.version;
        let job = Job::spawn(len, notify, move |ctx| {
            if fwd {
                inside(m.find_fwd(&snap, end, b, Some(ctx))).or_else(|| inside(m.find_fwd(&snap, a, end, Some(ctx))))
            } else {
                m.find_back(&snap, a, start, Some(ctx)).or_else(|| inside(m.find_back(&snap, start, b, Some(ctx))))
            }
        });
        self.find.find_job = Some((version, fwd, sel, job));
        self.flash("Searching…", false);
    }

    /// Find as you type: from where the current match (or the caret) starts, so typing more extends it.
    pub fn find_as_you_type(&mut self) {
        if self.find.matcher.is_none() {
            return;
        }
        let tab = &mut self.tabs[self.active];
        let start = tab.view.sel.start();
        tab.view.sel = Sel::at(start);
        self.find_step(true);
        if matches!(&self.flash, Some((m, _)) if m == "Search wrapped around") {
            self.flash = None;
        }
    }

    fn found(&mut self, hit: Option<(u64, u64)>, fwd: bool) {
        match hit {
            Some((a, b)) => {
                self.find.last_match = Some((a, b));
                let tab = &mut self.tabs[self.active];
                let wrapped = if fwd { a < tab.view.sel.end() } else { b > tab.view.sel.start() };
                tab.view.sel = Sel::new(a, b);
                tab.view.want_x = None;
                self.reveal_pending = true;
                self.reveal_center = true;
                if wrapped {
                    self.flash("Search wrapped around", false);
                } else {
                    self.flash = None;
                }
            }
            None => self.flash("No results", true),
        }
        self.dirty_view = true;
    }

    fn replace_one(&mut self) {
        if !self.editable() {
            return;
        }
        let Some(m) = self.find.matcher.as_ref() else { return };
        let tab = &self.tabs[self.active];
        let sel = tab.view.sel;
        if !sel.is_empty() && m.is_match_at(&tab.doc, sel.start(), sel.end()) {
            let hay = tab.doc.read(sel.start(), (sel.end() + 4096).min(tab.doc.len()));
            let mut out = Vec::new();
            let repl = edit::replacement(&self.find.replacement, self.find.query.regex, tab.doc.eol.as_bytes());
            m.expand(&hay, 0, &repl, &mut out);
            let tab = &mut self.tabs[self.active];
            tab.view.sel = edit::replace_selection(&mut tab.doc, sel, &out, EditKind::Other);
            tab.doc.seal();
            self.after_edit(true);
        }
        self.find_step(true);
    }

    /// Matches on screen: from the count when it's done for this text.
    pub fn visible_matches(&self, a: u64, b: u64) -> Vec<(u64, u64)> {
        let tab = self.tab();
        match &self.find.found {
            Some((v, f)) if self.find.open && *v == tab.doc.version => {
                let k = f.positions.partition_point(|p| p.1 < a);
                f.positions[k..].iter().take_while(|p| p.0 < b).copied().collect()
            }
            _ => Vec::new(),
        }
    }

    // ---- files changed by other programs ----

    /// Looks (on another thread) whether the tabs' files changed on disk.
    pub fn check_disk(&mut self) {
        if self.disk_job.is_some() {
            return;
        }
        // (a tab whose lines are still being read is looked at once they are: reading it again now would start over)
        let asks: Vec<(u64, PathBuf)> = self
            .tabs
            .iter()
            .filter(|t| !t.busy() && t.index.is_none() && t.doc.disk.is_some())
            .filter_map(|t| t.doc.path.clone().map(|p| (t.id, p)))
            .collect();
        if asks.is_empty() {
            return;
        }
        let notify = self.disk_notify.clone().unwrap_or_else(|| self.notify.clone());
        self.disk_job = Some(Job::spawn(0, notify, move |_| {
            asks.into_iter().map(|(id, p)| (id, fileio::disk_answer(&p).flatten())).collect()
        }));
    }

    pub fn poll_disk(&mut self) -> bool {
        let Some(job) = self.disk_job.as_mut() else { return false };
        let Some(found) = job.take() else { return true };
        self.disk_job = None;
        for (id, now) in found {
            let Some(i) = self.index_of(id) else { continue };
            let tab = &mut self.tabs[i];
            if tab.busy() || tab.index.is_some() || now == tab.doc.disk || tab.seen_disk == Some(now) {
                continue;
            }
            self.disk_found = true;
            let tab = &mut self.tabs[i];
            let name = tab.title();
            if now.is_none() {
                tab.seen_disk = Some(now);
                tab.notice = Some(format!("{name} isn't there any more (moved or deleted). Saving puts it back."));
                self.dirty_view = true;
                continue;
            }
            if !tab.doc.is_dirty() {
                // read again: the same tab, the view where it was (`loaded`); a file that only grew (a log) keeps the
                // lines counted so far
                let path = tab.doc.path.clone().unwrap();
                let notify = self.notify.clone();
                // (a re-read that fails isn't tried again until the file changes again; `loaded` forgets it)
                tab.seen_disk = Some(now);
                tab.goto = None;
                tab.reload = Some(tab.view.sel.caret >= tab.doc.len() && !tab.doc.is_empty());
                let prev = tab.doc.buffer().sources().iter().find(|s| s.file_path() == Some(path.as_path())).cloned();
                tab.load = Some(Job::spawn(fileio::OPEN_STEPS, notify.clone(), move |ctx| fileio::reload(&path, notify, prev, None, ctx)));
                self.flash(format!("{name} changed on disk and was reloaded."), false);
            } else {
                tab.seen_disk = Some(now);
                tab.notice = Some(format!("{name} changed on disk, and you have unsaved changes. Saving keeps yours."));
                self.dirty_view = true;
            }
        }
        false
    }
}

/// The language of a document, from its file's name and first bytes.
pub fn detect(path: Option<&Path>, doc: &Document) -> Lang {
    let head = doc.read(0, 4096.min(doc.len()));
    let name = path.map(|p| p.to_string_lossy().into_owned());
    Lang::detect(name.as_deref(), &head)
}

pub fn default_indent(s: &Settings) -> Indent {
    if s.use_spaces { Indent::Spaces(s.tab_size) } else { Indent::Tabs }
}

/// `notes.txt:120:5` from a command line: the file without the suffix, and the line and column (1 when there's
/// none). Only the name says so; whether a file of the full name is there is for the open to find out (it's that
/// file then). A name with other colons is just a name.
pub fn line_suffix(p: &Path) -> Option<(PathBuf, u64, u64)> {
    let name = p.file_name()?.to_str()?;
    let number = |s: &str| (!s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().ok()).flatten();
    let (rest, last) = name.rsplit_once(':')?;
    let last = number(last)?;
    let (base, line, col) = match rest.rsplit_once(':').and_then(|(b, l)| Some((b, number(l)?))) {
        Some((base, line)) => (base, line, last),
        None => (rest, last, 1),
    };
    if base.is_empty() {
        return None;
    }
    Some((p.with_file_name(base), line.max(1), col.max(1)))
}

fn guarded(snap: &Snapshot, f: impl FnOnce() -> TaskResult) -> TaskResult {
    let before = snap.read_errors();
    let r = f();
    if matches!(r, TaskResult::Cancelled) {
        return r;
    }
    if snap.read_errors() != before {
        return TaskResult::Failed("Part of the file couldn't be read (was it changed or removed?), so nothing was changed.".into());
    }
    if snap.changed_in_place() {
        return TaskResult::Failed("Another program changed this file while it was open, so nothing was changed. Reload it first.".into());
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_and_column_after_a_name() {
        let s = |p: &str| line_suffix(Path::new(p));
        assert_eq!(s("/no/such/notes.txt:120"), Some((PathBuf::from("/no/such/notes.txt"), 120, 1)));
        assert_eq!(s("/no/such/notes.txt:120:5"), Some((PathBuf::from("/no/such/notes.txt"), 120, 5)));
        assert_eq!(s("main.rs:7"), Some((PathBuf::from("main.rs"), 7, 1)));
        assert_eq!(s("/a/b:x:3"), Some((PathBuf::from("/a/b:x"), 3, 1)));
        for none in ["/no/such/a:b", "/a/b.txt", "/a/:12", "/a/b.txt:"] {
            assert_eq!(s(none), None, "{none}");
        }
    }
}
