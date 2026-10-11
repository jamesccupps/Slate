//! The application window's state, layout and painting. Input handling, commands and file work are in
//! `actions.rs`; the window procedure and message loop in `mod.rs`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_LINE_SPACING_METHOD_UNIFORM,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::core::document::{Document, Sel};
use crate::core::io::{SaveError, Saved};
use crate::core::job::{Job, Notify};
use crate::core::lines::LineOp;
use crate::core::search::Found;
use crate::core::source::Source;
use crate::core::text::{Encoding, Eol};
pub use crate::edit::{group, plural};

use super::commands::{Cmd, MENU_KEYS, MENU_TITLES};
use super::update::Release;
pub use super::editor::Indent;
use super::editor::{Counts, Ctx, Geom, Style, View};
use super::findbar::{self, FindBar};
use super::gfx::{Align, Gfx, Rect, font_info, rgb};
use super::highlight::Lang;
use super::settings::{Settings, ThemeMode};
use super::theme::{Theme, high_contrast_on, metrics, system_accent, system_prefers_dark};
use super::win;

pub type Cell = Rc<RefCell<App>>;

thread_local! {
    /// How many times the window was asked to repaint (test mode's `print:invalidated`).
    pub static INVALIDATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub struct SaveTask {
    pub job: Job<Result<Saved, SaveError>>,
    pub state: u64,
    pub version: u64,
    /// The text being saved, and which document it is (`Document::id`): a big file's text moves onto the saved
    /// file afterwards, also if it changed meanwhile, but only in that document.
    pub snap: crate::core::buffer::Snapshot,
    pub doc: u64,
    pub path: PathBuf,
    pub encoding: Encoding,
    /// Close the tab when done (unless the text changed meanwhile).
    pub close_after: bool,
    /// The text changed while saving and the user asked to save again: save the newer text when this is done.
    pub again: bool,
}

/// What a background look at a tab's file found (see `App::check_disk`).
pub struct DiskCheck {
    pub id: u64,
    /// The document's `disk` when the check started.
    pub old: crate::core::document::DiskInfo,
    pub now: Option<crate::core::document::DiskInfo>,
    /// Another program wrote into a file the (unsaved) text is read from: the text isn't the user's any more.
    pub in_place: bool,
    /// A file the (unsaved) text is read from was found not at its path any more (`Source::look_at_path`): the
    /// session has to be written again.
    pub gone: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskKind {
    Format(Fmt),
    Minify(Fmt),
    Validate(Fmt),
    ReplaceAll,
    Eol(Eol),
    Lines(LineOp),
}

/// The structured formats Slate can format and check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Json,
    Xml,
}

impl Fmt {
    pub fn name(self) -> &'static str {
        match self {
            Fmt::Json => "JSON",
            Fmt::Xml => "XML",
        }
    }
}

pub enum TaskResult {
    /// New content for the whole document.
    Content { src: Arc<Source>, nl: u64, count: u64 },
    /// The document checked out fine (with a summary).
    Valid(String),
    /// Not valid JSON / XML: where and why.
    FormatError { offset: u64, msg: String },
    Failed(String),
    Cancelled,
}

impl crate::core::job::Failure for TaskResult {
    fn failure(msg: &str) -> Self {
        TaskResult::Failed(msg.to_string())
    }
}

pub struct Task {
    pub kind: TaskKind,
    pub job: Job<TaskResult>,
    pub version: u64,
}

#[derive(Default)]
pub struct Search {
    /// The query and document version the results belong to.
    pub key: Option<(String, bool, bool, bool, u64)>,
    pub job: Option<Job<Found>>,
    pub found: Option<Found>,
    /// Where the scrollbar's match marks go, worked out once for a count and a bar (document version, matches, bar
    /// top and height): there can be a million matches, and the caret's blink repaints.
    pub marks: Option<((u64, usize, u32, u32), Vec<f32>)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeKind {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeAction {
    Reload,
    KeepMine,
    SaveAs,
    SaveUtf8,
    GoTo(u64),
    Dismiss,
    /// A tab from the session waiting for its file: look again now.
    Retry,
    /// A big document from the session waiting for its file: stop waiting, and get the text that was added to it.
    Recover,
}

pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    pub actions: Vec<(String, NoticeAction)>,
}

/// A search for the next or previous match on another thread: its result (start, end, whether it wrapped around),
/// what it's for (the document version and the selection when it started: it's dropped if either changed), and
/// whether it's the search as you type (which doesn't move where Find next goes on from, and says nothing).
pub struct FindJob {
    pub job: Job<Option<(u64, u64, bool)>>,
    pub version: u64,
    pub sel: Sel,
    pub live: bool,
}

pub struct Tab {
    pub id: u64,
    pub doc: Document,
    pub view: View,
    pub lang: Lang,
    /// The user picked `lang` (so it isn't guessed again).
    pub lang_picked: bool,
    pub untitled: u32,
    pub index_job: Option<Job<bool>>,
    /// Reading its file on another thread (opening or reloading it).
    pub load_job: Option<Load>,
    /// What `load_job` is: a reload (Some: whether the caret follows the end), else opening.
    pub reload: Option<bool>,
    /// A tab from the session whose file is being read: what the session says of it (kept as it is until then),
    /// and where its view goes once the file is read.
    pub place: Option<super::session::SessionTab>,
    pub save: Option<SaveTask>,
    pub task: Option<Task>,
    pub search: Search,
    /// A find next/previous or a search as you type running in the background (big documents, regexes).
    pub find_job: Option<FindJob>,
    pub notice: Option<Notice>,
    /// The session backup file of this tab (unique, so it never collides with another tab's from an earlier run)
    /// and the document version last written to it.
    pub backup_name: Option<String>,
    pub backup_version: u64,
    /// The on-disk state we already told the user about (so a change is announced once).
    pub seen_disk: Option<Option<crate::core::document::DiskInfo>>,
    /// Discard unsaved changes when closing (the user said "Don't save").
    pub discard: bool,
    /// JSON path bar and structure panel state.
    pub structure: super::structure::Structure,
    /// How this document is indented (worked out from its text, or picked in the menu); None: the settings' default.
    pub indent: Option<Indent>,
    /// The user picked `indent` (so it isn't guessed again, and wins over the rules for TSV files and makefiles).
    pub indent_picked: bool,
    /// A name for a tab that isn't a file (the keyboard shortcuts).
    pub title_override: Option<String>,
    /// A save stopped because ANSI can't hold some characters: (path, close after, closing the window), for the
    /// question that follows (`Deferred::AskLossy`).
    pub ask_lossy: Option<(PathBuf, bool, bool)>,
    /// The document's path and its canonical form, worked out once (for "is this file open already?").
    pub canon: Option<(PathBuf, Option<PathBuf>)>,
    /// Where to go once the document is ready and its tab is shown (`slate file.txt:120`, a reopened tab).
    pub goto: Option<Goto>,
    /// What the session holds of this document if it's too big for a copy (session.rs, "Big documents").
    pub big: Option<super::session::BigBackup>,
    /// A tab from the session that isn't back yet (being put back, or its file doesn't answer): see session.rs.
    pub restore: Option<super::session::Restoring>,
}

/// A tab's file being read on another thread (`Tab::load_job`).
pub enum Load {
    /// Reading it (`fileio::open`, `fileio::reload`): all of it if it's small, what it takes to show it if it's big.
    Read(Job<crate::core::io::Opened>),
    /// A big file in another encoding, being converted.
    Convert(Job<std::io::Result<Document>>),
}

impl Load {
    pub fn fraction(&self) -> f32 {
        match self {
            Load::Read(j) => j.fraction(),
            Load::Convert(j) => j.fraction(),
        }
    }
}

/// A place to go in a document that may not be ready yet (see `App::apply_goto`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Goto {
    /// A line (1-based) and column (characters, 1-based): needs the line index.
    Line(u64, Option<u64>),
    /// The caret and scroll position (bytes) a closed tab had.
    Place { caret: u64, top: u64 },
}

/// A tab closed with File → Close (or Ctrl+W...), for Reopen closed tab: its file and where it was in it.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedTab {
    pub path: PathBuf,
    pub caret: u64,
    pub top: u64,
}

impl Tab {
    pub fn new(id: u64, doc: Document) -> Tab {
        Tab {
            id,
            doc,
            view: View::new(),
            lang: Lang::Plain,
            lang_picked: false,
            untitled: 0,
            index_job: None,
            load_job: None,
            reload: None,
            place: None,
            save: None,
            task: None,
            search: Search::default(),
            find_job: None,
            notice: None,
            backup_name: None,
            backup_version: u64::MAX,
            seen_disk: None,
            discard: false,
            structure: Default::default(),
            indent: None,
            indent_picked: false,
            title_override: None,
            ask_lossy: None,
            canon: None,
            goto: None,
            big: None,
            restore: None,
        }
    }

    pub fn title(&self) -> String {
        if let Some(t) = &self.title_override {
            return t.clone();
        }
        match &self.doc.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None if self.untitled > 1 => format!("Untitled {}", self.untitled),
            None => "Untitled".into(),
        }
    }

    /// How far opening (or putting back from the session) is, while it is.
    pub fn opening(&self) -> Option<f32> {
        match (&self.load_job, self.restore.as_ref().and_then(|r| r.job.as_ref())) {
            (Some(j), _) => Some(j.fraction()),
            (None, Some(super::session::RestoreJob::Big(j))) => Some(j.fraction()),
            _ => None,
        }
    }

    pub fn busy(&self) -> bool {
        self.load_job.is_some() || self.restore.is_some() || self.save.is_some() || self.task.is_some()
    }

    pub fn is_blank(&self) -> bool {
        self.doc.path.is_none()
            && self.doc.is_empty()
            && !self.doc.is_dirty()
            && self.load_job.is_none()
            && self.restore.is_none()
    }
}

/// Things to do after the current message is handled (outside the App borrow, since they may open modal UI).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Deferred {
    Cmd(Cmd),
    Menu(usize),
    ContextMenu(f32, f32),
    TabMenu(usize, f32, f32),
    StatusMenu(StatusItem),
    /// "Slate x.y.z is available — update?"
    UpdatePrompt,
    /// Saving tab (id) as ANSI would turn some characters into "?": save as UTF-8, as ANSI anyway, or not.
    AskLossy(u64),
    /// The list of all tabs (the button at the end of a tab strip that overflows).
    TabList,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusItem {
    Position,
    Zoom,
    Eol,
    Encoding,
    Lang,
    Update,
    /// How the document is indented ("Spaces: 4"): opens the indentation menu.
    Indent,
    /// "OVR" while typing replaces characters (Insert); a click turns it off.
    Overtype,
}

/// Where updating Slate is at (see `update.rs`).
pub enum UpdateState {
    Idle,
    /// Asking GitHub; `manual`: from the Help menu (so say what came out of it).
    Checking { manual: bool, job: Job<Result<Release, String>> },
    Available(Release),
    Downloading { release: Release, job: Job<Result<(), String>> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    None,
    Tab(usize),
    TabClose(usize),
    NewTab,
    TabList,
    TabStrip,
    Menu(usize),
    ThemeToggle,
    Status(StatusItem),
    Find(findbar::Part),
    Notice(usize),
    PathSeg(usize),
    PathToggle,
    StructRow(usize, bool),
    StructClose,
    StructSplitter,
    /// The structure panel's scrollbar.
    StructBar,
    Gutter,
    Text,
    VBar,
    HBar,
}

pub struct UiFonts {
    pub ui: IDWriteTextFormat,
    pub ui_semibold: IDWriteTextFormat,
    pub icons: IDWriteTextFormat,
    pub icons_small: IDWriteTextFormat,
    pub family: String,
}

pub struct App {
    pub hwnd: HWND,
    pub g: Gfx,
    pub settings: Settings,
    pub theme: Theme,
    pub style: Style,
    pub fonts: UiFonts,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub next_id: u64,
    pub find: FindBar,
    pub dpi: u32,
    pub size: (f32, f32),
    pub r_tabs: Rect,
    pub r_menu: Rect,
    pub r_find: Rect,
    pub r_notice: Rect,
    pub r_edit: Rect,
    pub r_status: Rect,
    pub r_path: Rect,
    pub r_struct: Rect,
    /// Dragging the structure panel's edge: (pointer x at start, width at start).
    pub split_drag: Option<(f32, f32)>,
    /// Dragging the structure panel's scrollbar thumb: where in the thumb it was taken.
    pub struct_drag: Option<f32>,
    pub tab_rects: Vec<(Rect, Rect)>,
    pub newtab_rect: Rect,
    pub menu_rects: Vec<Rect>,
    pub theme_rect: Rect,
    pub status_rects: Vec<(StatusItem, Rect)>,
    pub notice_rects: Vec<Rect>,
    pub tab_scroll: f32,
    pub hover: Hit,
    /// What the left button was pressed on (drawn pressed while it's held there).
    pub down: Hit,
    /// What the middle button was pressed on (a tab closes when it's released on it).
    pub middle_down: Hit,
    pub tab_drag: Option<(usize, f32, bool)>,
    pub caret_on: bool,
    pub focused: bool,
    pub flash: Option<(String, Instant, bool)>,
    pub pending: Vec<Deferred>,
    pub notify: Notify,
    pub last_click: (Instant, f32, f32, u32),
    pub high_surrogate: Option<u16>,
    pub menu_open: Option<usize>,
    pub mono_fonts: Option<Vec<String>>,
    pub closing: bool,
    pub update: UpdateState,
    /// Start Slate again once this one has closed (a new version was just put in place).
    pub restart_on_exit: bool,
    pub untitled_counter: u32,
    pub session_dirty: bool,
    pub last_session_save: Instant,
    /// The session being written on another thread (while editing).
    pub session_job: Option<Job<super::session::Outcome>>,
    /// The window was drawn once (the tabs from the session are shown: `session::restored`).
    pub painted: bool,
    pub mouse_tracking: bool,
    /// `g.generation` the cached layouts were made for.
    pub gfx_generation: u64,
    /// Alt was pressed and released on its own: the menu bar has the keyboard, with this title highlighted.
    pub menu_armed: Option<usize>,
    /// Mouse wheel movement not scrolled yet (in rows; touchpads send small steps), and for Ctrl+wheel zoom (in
    /// wheel units).
    pub wheel_rows: f32,
    pub wheel_zoom: i32,
    /// Copy or Cut with nothing selected put a whole line on the clipboard: its clipboard sequence number, so a paste
    /// of that line goes in as a line above the caret's.
    pub line_clip: Option<u32>,
    /// The flash message came from searching (cleared when the search changes).
    pub flash_search: bool,
    /// Looking at the open files on disk (a network drive that went away can take long to answer).
    pub disk_job: Option<Job<Vec<DiskCheck>>>,
    /// Typing replaces the character after the caret (Insert toggles it).
    pub overtype: bool,
    /// Tabs closed recently (with a file), the last one closed last; Ctrl+Shift+T opens them again.
    pub closed_tabs: Vec<ClosedTab>,
    /// The button listing all tabs, at the end of the tab strip (zero size while all tabs fit).
    pub tablist_rect: Rect,
    /// Characters and words of a document (tab id, document version), and a count running on another thread.
    pub doc_counts: Option<(u64, u64, Counts)>,
    pub count_job: Option<(Job<Option<Counts>>, u64, u64)>,
    /// The same for the selection: (document version, start, end).
    pub sel_counts: Option<((u64, u64, u64), Counts)>,
    /// The document version a recount waits to start for, and since when (see `doc_counts_now`).
    pub count_wait: Option<(u64, Instant)>,
    /// The tooltip of what the mouse rests on, and when one last went away (the next one then comes at once).
    pub tip: win::Tip,
    pub tip_gone: Option<Instant>,
    /// The caret's line and column, as the status bar last worked them out.
    pub caret_place: CaretPlace,
}

pub use crate::settings::ZOOM_STEPS;

fn pick_family(dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, wanted: &[&str]) -> String {
    for f in wanted {
        if font_info(dw, f, 12.0).is_some() {
            return f.to_string();
        }
    }
    wanted.last().unwrap().to_string()
}

impl App {
    pub fn new(hwnd: HWND, notify: Notify) -> App {
        let g = Gfx::new().expect("Direct2D is not available");
        let settings = Settings::load();
        let theme = make_theme(settings.theme);
        let dpi = win::dpi_of(hwnd);
        let fonts = make_fonts(&g);
        let style = make_style(&g, &settings, 1);
        let mut find = FindBar::new(hwnd);
        find.style_edits(dpi, &theme);
        let mut app = App {
            hwnd,
            g,
            settings,
            theme,
            style,
            fonts,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            find,
            dpi,
            size: (800.0, 600.0),
            r_tabs: Rect::default(),
            r_menu: Rect::default(),
            r_find: Rect::default(),
            r_notice: Rect::default(),
            r_edit: Rect::default(),
            r_status: Rect::default(),
            r_path: Rect::default(),
            r_struct: Rect::default(),
            split_drag: None,
            struct_drag: None,
            tab_rects: Vec::new(),
            newtab_rect: Rect::default(),
            menu_rects: Vec::new(),
            theme_rect: Rect::default(),
            status_rects: Vec::new(),
            notice_rects: Vec::new(),
            tab_scroll: 0.0,
            hover: Hit::None,
            down: Hit::None,
            middle_down: Hit::None,
            tab_drag: None,
            caret_on: true,
            focused: true,
            flash: None,
            pending: Vec::new(),
            notify,
            last_click: (Instant::now(), -100.0, -100.0, 0),
            high_surrogate: None,
            menu_open: None,
            mono_fonts: None,
            closing: false,
            update: UpdateState::Idle,
            restart_on_exit: false,
            untitled_counter: 0,
            session_dirty: false,
            last_session_save: Instant::now(),
            session_job: None,
            painted: false,
            mouse_tracking: false,
            gfx_generation: 0,
            menu_armed: None,
            wheel_rows: 0.0,
            wheel_zoom: 0,
            line_clip: None,
            flash_search: false,
            disk_job: None,
            overtype: false,
            closed_tabs: Vec::new(),
            tablist_rect: Rect::default(),
            doc_counts: None,
            count_job: None,
            sel_counts: None,
            count_wait: None,
            tip: win::Tip::new(hwnd),
            tip_gone: None,
            caret_place: CaretPlace::default(),
        };
        app.apply_theme();
        app
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn invalidate(&self) {
        INVALIDATED.with(|n| n.set(n.get() + 1));
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    pub fn flash(&mut self, msg: impl Into<String>, bad: bool) {
        self.flash = Some((msg.into(), Instant::now(), bad));
        self.flash_search = false;
        // (The caret's blinking no longer repaints an unfocused window: take the message away on time.)
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(self.hwnd, super::actions::TIMER_FLASH, 6100, None);
        }
        self.invalidate();
    }

    /// A message about the search ("No results"...), which goes away once the search changes.
    pub fn flash_search(&mut self, msg: impl Into<String>, bad: bool) {
        self.flash(msg, bad);
        self.flash_search = true;
    }

    pub fn new_tab_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Re-reads the theme (settings or Windows changed).
    pub fn apply_theme(&mut self) {
        self.theme = make_theme(self.settings.theme);
        if self.theme.hc {
            win::system_title_bar(self.hwnd);
        } else {
            win::style_title_bar(self.hwnd, self.theme.dark, self.theme.frame, self.theme.text);
        }
        win::set_menu_dark(self.theme.dark && !self.theme.hc);
        self.tip.set_dark(self.theme.dark && !self.theme.hc);
        let t = &self.theme;
        super::prompt::set_colors((!t.hc).then_some(super::prompt::Colors {
            dark: t.dark,
            surface: t.surface,
            frame: t.frame,
            text: t.text,
            border: t.border,
        }));
        self.find.style_edits(self.dpi, &self.theme);
        self.rebuild_style();
    }

    /// Rebuilds fonts and text formats (font, size, zoom, wrap or tab settings changed).
    pub fn rebuild_style(&mut self) {
        let generation = self.style.generation + 1;
        self.style = make_style(&self.g, &self.settings, generation);
        for t in &mut self.tabs {
            t.view.clear_cache();
            t.view.content_w = 0.0;
        }
        self.invalidate();
    }

    pub fn update_dpi(&mut self) {
        self.dpi = win::dpi_of(self.hwnd);
        self.find.style_edits(self.dpi, &self.theme);
        self.rebuild_style();
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
    }

    pub fn px_to_dip(&self, v: i32) -> f32 {
        v as f32 * 96.0 / self.dpi as f32
    }

    pub fn dip_to_px(&self, v: f32) -> i32 {
        (v * self.dpi as f32 / 96.0).round() as i32
    }

    // ---- layout ----

    pub fn layout(&mut self) {
        let (w, h) = self.client_size();
        let (w, h) = (self.px_to_dip(w as i32), self.px_to_dip(h as i32));
        self.size = (w, h);
        // Every band starts and ends on a device pixel, so its edges and the hairlines along them stay crisp at 125%
        // and 150% too.
        let dpi = self.dpi as f32;
        let snap = |v: f32| super::gfx::snap(v, dpi);
        let band = |y: f32, bh: f32| Rect::new(0.0, snap(y), w, snap(y + bh) - snap(y));
        let mut y = 0.0;
        self.r_tabs = band(y, metrics::TAB_BAR_H);
        y += metrics::TAB_BAR_H;
        self.r_menu = band(y, metrics::MENU_BAR_H);
        y += metrics::MENU_BAR_H;
        let fh = self.find.height();
        self.r_find = band(y, fh);
        y += fh;
        let nh = if self.tabs.get(self.active).is_some_and(|t| t.notice.is_some()) { metrics::NOTICE_H } else { 0.0 };
        self.r_notice = band(y, nh);
        y += nh;
        let json = self.tabs.get(self.active).is_some_and(|t| t.lang.has_structure());
        let ph = if json && self.settings.path_bar { super::structure::PATH_H } else { 0.0 };
        self.r_path = band(y, ph);
        y = snap(y + ph);
        let sh = metrics::STATUS_H;
        let sy = snap((h - sh).max(y));
        self.r_status = Rect::new(0.0, sy, w, (h - sy).max(sh));
        let body_h = (sy - y).max(0.0);
        if json && self.settings.structure_panel {
            let pw = self.settings.structure_width.clamp(200.0, (w * 0.6).max(200.0));
            let x = snap((w - pw).max(0.0));
            self.r_struct = Rect::new(x, y, w - x, body_h);
            self.r_edit = Rect::new(0.0, y, x, body_h);
        } else {
            self.r_struct = Rect::default();
            self.r_edit = Rect::new(0.0, y, w, body_h);
        }
        let ui = self.fonts.ui.clone();
        self.find.layout(self.r_find, &self.g, &ui);
        self.layout_tabs();
        self.layout_menu();
    }

    fn layout_tabs(&mut self) {
        self.tab_rects.clear();
        let bar = self.r_tabs;
        let left = 8.0;
        let new_w = 36.0;
        let mut avail = (bar.w - left - new_w - 16.0).max(metrics::TAB_MIN_W);
        // Tabs that don't all fit: a button after the new-tab one lists them all.
        let overflow = self.tabs.len() as f32 * metrics::TAB_MIN_W > avail;
        if overflow {
            avail = (avail - 32.0).max(metrics::TAB_MIN_W);
        }
        let n = self.tabs.len().max(1) as f32;
        let tw = (avail / n).clamp(metrics::TAB_MIN_W, metrics::TAB_MAX_W);
        let total = tw * self.tabs.len() as f32;
        let max_scroll = (total - avail).max(0.0);
        self.tab_scroll = self.tab_scroll.clamp(0.0, max_scroll);
        let top = super::gfx::snap(bar.y + 6.0, self.dpi as f32);
        let h = bar.bottom() - top;
        for i in 0..self.tabs.len() {
            let x = bar.x + left + i as f32 * tw - self.tab_scroll;
            let r = Rect::new(x, top, tw, h);
            let c = Rect::new(r.right() - 32.0, top + (h - 24.0) / 2.0, 24.0, 24.0);
            self.tab_rects.push((r, c));
        }
        let nx = (bar.x + left + total - self.tab_scroll).min(bar.x + left + avail) + 4.0;
        self.newtab_rect = Rect::new(nx, top + (h - 28.0) / 2.0, 28.0, 28.0);
        self.tablist_rect = if overflow { Rect::new(nx + 32.0, self.newtab_rect.y, 28.0, 28.0) } else { Rect::default() };
    }

    /// Where tabs stop being drawn (and clicked): just before the new-tab button, which follows the last tab or,
    /// when the tabs don't all fit, stays at the end of the strip.
    fn tabs_clip_right(&self) -> f32 {
        self.newtab_rect.x - 4.0
    }

    /// Scrolls the tab strip so the active tab is visible.
    pub fn reveal_active_tab(&mut self) {
        self.layout_tabs();
        if let Some((r, _)) = self.tab_rects.get(self.active) {
            let bar = self.r_tabs;
            let left = bar.x + 8.0;
            let right = self.tabs_clip_right();
            if r.x < left {
                self.tab_scroll -= left - r.x;
            } else if r.right() > right {
                self.tab_scroll += r.right() - right;
            }
            self.layout_tabs();
        }
    }

    fn layout_menu(&mut self) {
        self.menu_rects.clear();
        let mut x = self.r_menu.x + 6.0;
        for t in MENU_TITLES {
            let (w, _) = self.g.measure(t, &self.fonts.ui);
            let r = Rect::new(x, self.r_menu.y + 3.0, w + 20.0, self.r_menu.h - 6.0);
            self.menu_rects.push(r);
            x += w + 22.0;
        }
        // (In high contrast Windows' colors are used whatever the theme: no light/dark switch.)
        self.theme_rect = if self.theme.hc {
            Rect::default()
        } else {
            Rect::new(self.r_menu.right() - 44.0, self.r_menu.y + 3.0, 34.0, self.r_menu.h - 6.0)
        };
    }

    pub fn editor_geom(&self) -> Geom {
        View::geometry(&self.tab().doc, &self.style, self.r_edit)
    }

    // ---- hit testing ----

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        if self.r_tabs.contains(x, y) {
            for (i, (r, c)) in self.tab_rects.iter().enumerate() {
                if r.contains(x, y) && x >= self.r_tabs.x && x < self.tabs_clip_right() {
                    if c.contains(x, y) {
                        return Hit::TabClose(i);
                    }
                    return Hit::Tab(i);
                }
            }
            if self.newtab_rect.contains(x, y) {
                return Hit::NewTab;
            }
            if self.tablist_rect.contains(x, y) {
                return Hit::TabList;
            }
            return Hit::TabStrip;
        }
        if self.r_menu.contains(x, y) {
            if self.theme_rect.contains(x, y) {
                return Hit::ThemeToggle;
            }
            for (i, r) in self.menu_rects.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::Menu(i);
                }
            }
            return Hit::None;
        }
        if self.r_find.contains(x, y) {
            return self.find.hit(x, y).map(Hit::Find).unwrap_or(Hit::None);
        }
        if self.r_notice.contains(x, y) {
            for (i, r) in self.notice_rects.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::Notice(i);
                }
            }
            return Hit::None;
        }
        if self.r_path.contains(x, y) {
            if super::structure::toggle_rect(self.r_path).contains(x, y) {
                return Hit::PathToggle;
            }
            let st = &self.tab().structure;
            for (i, r) in st.path_rects.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::PathSeg(i);
                }
            }
            return Hit::None;
        }
        if self.r_struct.w > 0.0 && self.r_struct.contains(x, y) {
            if x < self.r_struct.x + 5.0 {
                return Hit::StructSplitter;
            }
            if super::structure::close_rect(self.r_struct).contains(x, y) {
                return Hit::StructClose;
            }
            if self.tab().structure.scrollbar(self.r_struct).is_some_and(|(track, _, _)| track.contains(x, y)) {
                return Hit::StructBar;
            }
            return match self.tab().structure.row_at(self.r_struct, x, y) {
                Some((i, chevron)) => Hit::StructRow(i, chevron),
                None => Hit::None,
            };
        }
        if self.r_status.contains(x, y) {
            for (item, r) in &self.status_rects {
                if r.contains(x, y) {
                    return Hit::Status(*item);
                }
            }
            return Hit::None;
        }
        if self.r_edit.contains(x, y) && !self.tabs.is_empty() {
            let geom = self.editor_geom();
            if geom.vbar().contains(x, y) {
                return Hit::VBar;
            }
            if !self.style.wrap && self.tab().view.content_w > geom.text_w && geom.hbar().contains(x, y) {
                return Hit::HBar;
            }
            if x < geom.text_x - 2.0 {
                return Hit::Gutter;
            }
            return Hit::Text;
        }
        Hit::None
    }

    /// What the tooltip of `h` says: what a button with a symbol does (and its key), a tab's file (or its whole name
    /// when it's cut short). None for parts whose words say it.
    pub fn tip_text(&self, h: Hit) -> Option<String> {
        use findbar::{Mode, Part};
        Some(
            match h {
                Hit::Tab(i) => {
                    let t = self.tabs.get(i)?;
                    if let Some(p) = t.doc.path.as_ref().filter(|_| t.title_override.is_none()) {
                        return Some(p.display().to_string());
                    }
                    let label = self.tab_label(i);
                    if self.g.measure(&label, &self.fonts.ui).0 <= self.tab_title_rect(i).w {
                        return None;
                    }
                    return Some(label);
                }
                Hit::TabClose(_) => "Close tab (Ctrl+W)",
                Hit::NewTab => "New tab (Ctrl+T)",
                Hit::TabList => "All tabs",
                Hit::ThemeToggle if self.theme.dark => "Light theme",
                Hit::ThemeToggle => "Dark theme",
                Hit::PathToggle => "Structure panel (Ctrl+Shift+O)",
                Hit::StructClose => "Close the structure panel (Ctrl+Shift+O)",
                Hit::Find(Part::Expand) if self.find.mode == Mode::Replace => "Hide replace",
                Hit::Find(Part::Expand) => "Replace (Ctrl+H)",
                Hit::Find(Part::Case) => "Match case (Alt+C)",
                Hit::Find(Part::Word) => "Whole word (Alt+W)",
                Hit::Find(Part::Regex) => "Regular expression (Alt+R)",
                Hit::Find(Part::Prev) => "Previous match (Shift+F3)",
                Hit::Find(Part::Next) => "Next match (F3)",
                Hit::Find(Part::Close) => "Close (Esc)",
                Hit::Find(Part::ReplaceAll) => "Replace all (Ctrl+Alt+Enter)",
                Hit::Status(StatusItem::Position) => "Go to line (Ctrl+G)",
                Hit::Status(StatusItem::Zoom) => "Reset zoom (Ctrl+0)",
                Hit::Status(StatusItem::Eol) => "Line endings",
                Hit::Status(StatusItem::Encoding) => "Encoding",
                Hit::Status(StatusItem::Lang) => "Language",
                Hit::Status(StatusItem::Indent) => "Indentation",
                Hit::Status(StatusItem::Overtype) => "Typing replaces characters (Insert turns it off)",
                _ => return None,
            }
            .to_string(),
        )
    }

    /// Where the part `h` is (client DIPs), for its tooltip.
    pub fn hit_rect(&self, h: Hit) -> Option<Rect> {
        Some(match h {
            Hit::Tab(i) => self.tab_rects.get(i)?.0,
            Hit::TabClose(i) => self.tab_rects.get(i)?.1,
            Hit::NewTab => self.newtab_rect,
            Hit::TabList => self.tablist_rect,
            Hit::ThemeToggle => self.theme_rect,
            Hit::PathToggle => super::structure::toggle_rect(self.r_path),
            Hit::StructClose => super::structure::close_rect(self.r_struct),
            Hit::Find(p) => self.find.parts.iter().find(|(q, _)| *q == p)?.1,
            Hit::Status(s) => self.status_rects.iter().find(|(q, _)| *q == s)?.1,
            _ => return None,
        })
    }

    // ---- painting ----

    pub fn paint(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let (w, h) = self.client_size();
        if self.g.ensure_target(self.hwnd, w, h, self.dpi as f32).is_err() {
            return;
        }
        // Cached text layouts hold brushes of the old target.
        if self.g.generation != self.gfx_generation {
            self.gfx_generation = self.g.generation;
            for t in &mut self.tabs {
                t.view.clear_cache();
            }
            self.style.generation += 1;
        }
        self.layout();
        self.update_structure();
        let rt = self.g.rt().clone();
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&super::gfx::color(self.theme.frame) as *const D2D1_COLOR_F));
        }
        self.paint_tabs();
        self.paint_menu();
        let f = win::focus();
        if !self.g.offscreen {
            // Asked here rather than tracked: focus moved while the app was busy never reaches WM_SETFOCUS.
            self.focused = f == self.hwnd;
        }
        let focused_edit = if self.find.is_edit(f) { Some(f) } else { None };
        let hover_part = if let Hit::Find(p) = self.hover { Some(p) } else { None };
        let pressed = hover_part.is_some() && self.down == self.hover;
        let (ui, icons) = (&self.fonts.ui, &self.fonts.icons_small);
        self.find.paint(&self.g, &self.theme, self.r_find, ui, icons, hover_part, pressed, focused_edit);
        self.paint_notice();
        self.paint_editor(focused_edit.is_none());
        self.paint_structure();
        self.paint_status();
        drop(rt);
        if !self.g.present() {
            // The graphics device was lost (driver update, GPU reset...): start over with a fresh target.
            self.g.discard_target();
            self.invalidate();
        }
        if !self.painted {
            self.painted = true;
            if !self.g.offscreen {
                super::session::restored();
            }
        }
    }

    /// What tab `i` is called on the tab strip: its title, plus as much of its folder as tells it apart from other
    /// tabs with the same name ("config.json — server").
    pub fn tab_label(&self, i: usize) -> String {
        let tab = &self.tabs[i];
        let title = tab.title();
        let Some(path) = tab.doc.path.as_ref().filter(|_| tab.title_override.is_none()) else { return title };
        // folder names from the file upwards
        let dirs = |p: &std::path::Path| -> Vec<String> {
            let parent = p.parent().map(|d| d.components().collect::<Vec<_>>()).unwrap_or_default();
            parent
                .iter()
                .rev()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
                    _ => None,
                })
                .collect()
        };
        let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let others: Vec<Vec<String>> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(j, t)| *j != i && t.title_override.is_none())
            .filter_map(|(_, t)| t.doc.path.as_deref())
            .filter(|p| name(p) == name(path))
            .map(dirs)
            .collect();
        if others.is_empty() {
            return title;
        }
        let mine = dirs(path);
        let shown: Vec<String> = path
            .parent()
            .map(|d| {
                d.components()
                    .rev()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let k = (1..=mine.len()).find(|&k| others.iter().all(|o| o.get(..k) != mine.get(..k))).unwrap_or(mine.len());
        if k == 0 {
            return title;
        }
        let folder: Vec<&str> = shown[..k].iter().rev().map(String::as_str).collect();
        format!("{title} — {}", folder.join("\\"))
    }

    fn paint_tabs(&mut self) {
        let t = self.theme.clone();
        let g = &self.g;
        g.fill(self.r_tabs, t.frame);
        let clip = Rect::new(self.r_tabs.x, self.r_tabs.y, self.tabs_clip_right() - self.r_tabs.x, self.r_tabs.h);
        g.push_clip(clip);
        for (i, (r, c)) in self.tab_rects.iter().enumerate() {
            if r.right() < clip.x || r.x > clip.right() {
                continue;
            }
            let tab = &self.tabs[i];
            let active = i == self.active;
            let hovered = matches!(self.hover, Hit::Tab(j) | Hit::TabClose(j) if j == i);
            let next_hovered = matches!(self.hover, Hit::Tab(j) | Hit::TabClose(j) if j == i + 1);
            if active {
                // Rounded top corners; the bottom merges into the menu bar.
                g.fill_round(Rect::new(r.x, r.y, r.w, r.h + 10.0), 7.0, t.surface);
                if t.hc {
                    // (In high contrast the strip has the tab's color: outline the active tab instead.)
                    let outline = g.snap_rect(Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 3.0));
                    g.stroke_round(outline, 6.0, t.accent, 2.0 * g.hair());
                }
            } else if hovered {
                g.fill_round(Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 6.0), 6.0, t.hover);
            } else if i + 1 != self.active && i + 1 < self.tabs.len() && !next_hovered {
                // separator between inactive tabs (not next to the one under the mouse)
                g.vline(r.right(), r.y + 10.0, r.bottom() - 10.0, true, t.border);
            }
            let busy = tab.busy() || !tab.doc.is_ready();
            let title = self.tab_label(i);
            let show_close = hovered || active;
            // (the unsaved-changes dot next to the close button on the active tab)
            let dot = active && tab.doc.is_dirty() && self.hover != Hit::TabClose(i);
            let color = if active { t.text } else { t.text_dim };
            g.text(&title, &self.fonts.ui, self.tab_title_rect(i), color, Align::Left);
            if show_close {
                if let Some(bg) = self.hot(Hit::TabClose(i)) {
                    g.fill_round(*c, 4.0, bg);
                }
                g.text("\u{E711}", &self.fonts.icons_small, *c, t.text_dim, Align::Center);
            } else if tab.doc.is_dirty() {
                let d = 8.0;
                g.fill_round(Rect::new(c.x + (c.w - d) / 2.0, c.y + (c.h - d) / 2.0, d, d), d / 2.0, t.text_dim);
            }
            if dot {
                // dirty dot shown next to the close button on the active tab
                let d = 6.0;
                g.fill_round(Rect::new(c.x - 10.0, c.y + (c.h - d) / 2.0, d, d), d / 2.0, t.text_dim);
            }
            if busy {
                let pct = tab_progress(tab);
                let bw = (r.w - 28.0) * pct.unwrap_or(0.35);
                g.fill(g.snap_rect(Rect::new(r.x + 14.0, r.bottom() - 4.0, bw.max(4.0), 2.0)), t.accent);
            }
        }
        g.pop_clip();
        let nr = self.newtab_rect;
        if let Some(bg) = self.hot(Hit::NewTab) {
            g.fill_round(nr, 5.0, bg);
        }
        g.text("\u{E710}", &self.fonts.icons_small, nr, t.text_dim, Align::Center);
        let lr = self.tablist_rect;
        if lr.w > 0.0 {
            if let Some(bg) = self.hot(Hit::TabList) {
                g.fill_round(lr, 5.0, bg);
            }
            g.text("\u{E70D}", &self.fonts.icons_small, lr, t.text_dim, Align::Center);
        }
    }

    /// Where tab `i`'s title goes: all of the tab but what shows at its end (the close button, the unsaved-changes
    /// dot), so a narrow tab still shows most of its name.
    fn tab_title_rect(&self, i: usize) -> Rect {
        let (r, c) = self.tab_rects[i];
        let active = i == self.active;
        let hovered = matches!(self.hover, Hit::Tab(j) | Hit::TabClose(j) if j == i);
        let dirty = self.tabs[i].doc.is_dirty();
        let end = if active && dirty && self.hover != Hit::TabClose(i) {
            c.x - 16.0
        } else if active || hovered {
            c.x - 4.0
        } else if dirty {
            c.x + 2.0
        } else {
            r.right() - 12.0
        };
        Rect::new(r.x + 14.0, r.y, (end - r.x - 14.0).max(10.0), r.h - 2.0)
    }

    /// The background of a button under the mouse: darker while it's held down on it.
    fn hot(&self, h: Hit) -> Option<u32> {
        if self.hover != h {
            None
        } else if self.down == h {
            Some(self.theme.pressed)
        } else {
            Some(self.theme.hover)
        }
    }

    fn paint_menu(&mut self) {
        let t = &self.theme;
        let g = &self.g;
        g.fill(self.r_menu, t.surface);
        for (i, r) in self.menu_rects.iter().enumerate() {
            if self.menu_open == Some(i) {
                g.fill_round(*r, 5.0, t.pressed);
            } else if self.hover == Hit::Menu(i) || self.menu_armed == Some(i) {
                g.fill_round(*r, 5.0, t.hover);
            }
            if self.menu_armed.is_some() {
                // The keyboard is on the menu bar: underline the letter that opens each menu.
                let l = g.layout(&super::gfx::wide(MENU_TITLES[i]), &self.fonts.ui, r.w, r.h);
                unsafe {
                    let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                    let _ = l.SetUnderline(true, DWRITE_TEXT_RANGE { startPosition: MENU_KEYS[i], length: 1 });
                }
                g.draw_layout(&l, r.x, r.y, t.text);
            } else {
                g.text(MENU_TITLES[i], &self.fonts.ui, *r, t.text, Align::Center);
            }
        }
        // Light/dark switch: a sun in dark mode, a moon in light mode.
        if let Some(bg) = self.hot(Hit::ThemeToggle) {
            g.fill_round(self.theme_rect, 5.0, bg);
        }
        let glyph = if t.dark { "\u{E706}" } else { "\u{E708}" };
        if self.theme_rect.w > 0.0 {
            g.text(glyph, &self.fonts.icons, self.theme_rect, t.text_dim, Align::Center);
        }
        if self.find.open || self.tab().notice.is_some() {
            return;
        }
        g.hline(0.0, self.r_menu.right(), self.r_menu.bottom(), true, t.border);
    }

    fn paint_notice(&mut self) {
        self.notice_rects.clear();
        let r = self.r_notice;
        if r.h <= 0.0 {
            return;
        }
        let t = self.theme.clone();
        let Some(n) = self.tabs[self.active].notice.as_ref() else { return };
        let g = &self.g;
        g.fill(r, t.notice_bg);
        g.hline(r.x, r.right(), r.bottom(), true, t.border);
        let icon = match n.kind {
            NoticeKind::Info => "\u{E946}",
            NoticeKind::Warn => "\u{E7BA}",
            NoticeKind::Error => "\u{EA39}",
        };
        let ic = match n.kind {
            NoticeKind::Info => t.accent,
            NoticeKind::Warn => t.warning,
            NoticeKind::Error => t.error,
        };
        g.text(icon, &self.fonts.icons_small, Rect::new(r.x + 10.0, r.y, 20.0, r.h), ic, Align::Center);
        let mut x = r.right() - 8.0;
        let mut rects = Vec::new();
        for (label, _) in n.actions.iter().rev() {
            let (w, _) = g.measure(label, &self.fonts.ui);
            let br = Rect::new(x - w - 20.0, r.y + 5.0, w + 20.0, r.h - 10.0);
            rects.push(br);
            x = br.x - 6.0;
        }
        rects.reverse();
        for (i, br) in rects.iter().enumerate() {
            let hovered = self.hover == Hit::Notice(i);
            g.fill_round(*br, 4.0, if hovered { t.pressed } else { t.hover });
            if hovered && self.down == self.hover {
                // (held down: darker still)
                g.fill_round(*br, 4.0, t.hover);
            }
            g.text(&n.actions[i].0, &self.fonts.ui, *br, t.text, Align::Center);
        }
        g.text(&n.text, &self.fonts.ui, Rect::new(r.x + 36.0, r.y, (x - r.x - 40.0).max(20.0), r.h), t.text, Align::Left);
        self.notice_rects = rects;
    }

    fn paint_structure(&mut self) {
        if self.r_path.h <= 0.0 && self.r_struct.w <= 0.0 {
            return;
        }
        let t = self.theme.clone();
        let (ui, bold, icons) = (self.fonts.ui.clone(), self.fonts.ui_semibold.clone(), self.fonts.icons_small.clone());
        let hover = self.hover;
        let panel_open = self.r_struct.w > 0.0;
        let (r_path, r_struct) = (self.r_path, self.r_struct);
        let tab = &mut self.tabs[self.active];
        if r_path.h > 0.0 {
            let seg_hover = if let Hit::PathSeg(i) = hover { Some(i) } else { None };
            tab.structure.paint_path(&self.g, &t, &ui, r_path, seg_hover, hover == Hit::PathToggle, panel_open, &icons);
        }
        if panel_open {
            let row_hover = if let Hit::StructRow(i, _) = hover { Some(i) } else { None };
            let (close_hot, bar_hot) = (hover == Hit::StructClose, hover == Hit::StructBar || self.struct_drag.is_some());
            tab.structure.paint_panel(&self.g, &t, &ui, &bold, &icons, r_struct, row_hover, close_hot, bar_hot);
            if hover == Hit::StructSplitter || self.split_drag.is_some() {
                self.g.fill(self.g.snap_rect(Rect::new(r_struct.x, r_struct.y, 2.0, r_struct.h)), t.accent);
            }
        }
    }

    /// Keeps the JSON or XML path (and the panel's rows) up to date with the caret.
    pub fn update_structure(&mut self) {
        let json = self.tabs.get(self.active).is_some_and(|t| t.lang.has_structure());
        if !json || (self.r_path.h <= 0.0 && self.r_struct.w <= 0.0) {
            return;
        }
        let panel = self.r_struct.w > 0.0;
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let caret = tab.view.sel.caret;
        tab.structure.set_lang(tab.lang);
        if tab.structure.waiting(&tab.doc) {
            // (typing: worked out once it pauses)
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    self.hwnd,
                    super::actions::TIMER_STRUCTURE,
                    super::structure::PAUSE.as_millis() as u32 + 10,
                    None,
                );
            }
            return;
        }
        tab.structure.follow = panel;
        let before = tab.structure.selected;
        tab.structure.update_path(&mut tab.doc, caret, &notify);
        if panel {
            tab.structure.rows(&mut tab.doc, &notify);
            if tab.structure.selected != before {
                tab.structure.scroll_to_selected(self.r_struct);
            }
        }
        if tab.structure.busy() {
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(self.hwnd, super::actions::TIMER_JOBS, 100, None);
            }
        }
    }

    fn paint_editor(&mut self, focused: bool) {
        let geom = self.editor_geom();
        let caret_on = self.caret_on && self.focused;
        let (hwnd, k) = (self.hwnd, self.dpi as f32 / 96.0);
        let matcher = if self.find.open && self.find.mode != findbar::Mode::GoTo { self.find.matcher.clone() } else { None };
        let tab = &mut self.tabs[self.active];
        tab.view.sync(&mut tab.doc);
        let cx = Ctx { doc: &tab.doc, g: &self.g, style: &self.style, theme: &self.theme, lang: tab.lang, geom };
        tab.view.layout_rows(&cx);
        // Matches on screen (searched directly; cheap for a screenful of text).
        let mut matches = Vec::new();
        if let (Some(m), Some(first), Some(last)) = (matcher, tab.view.rows.first(), tab.view.rows.last()) {
            let a = first.start.saturating_sub(256);
            let b = (last.end + 256).min(tab.doc.len());
            if b - a < 4 << 20 {
                let bytes = tab.doc.read(a, b);
                matches = m.matches_in(&bytes, a, 5000);
            }
        }
        let editor_focused = focused && self.focused;
        tab.view.paint(&cx, editor_focused, caret_on, &matches, self.overtype);
        if editor_focused {
            // The hidden system caret goes where Slate's is (Magnifier and screen readers follow that one). While the
            // caret is scrolled out of view there's none, so they don't stay on a place it has left.
            let (left, right) = (geom.text_x - 2.0, geom.text_x + geom.text_w + 2.0);
            match tab.view.caret_rect(&cx, self.overtype).filter(|c| c.right() > left && c.x < right) {
                Some(c) => {
                    let px = |v: f32| (v * k).round() as i32;
                    win::follow_caret(hwnd, px(c.x), px(c.y), px(c.w).max(1), px(c.h).max(1));
                }
                None => win::drop_caret(),
            }
        }
        // Loading / converting overlay.
        if let Some(f) = tab.opening() {
            let msg = format!("Opening… {:.0}%", f * 100.0);
            self.g.text(&msg, &self.fonts.ui, Rect::new(geom.text_x, geom.rect.y + 8.0, 400.0, 24.0), self.theme.text_dim, Align::Left);
        }
        // Scrollbars.
        let t = &self.theme;
        let vb = geom.vbar();
        let (frac, shown) = tab.view.scroll_fraction(&cx);
        if shown < 1.0 || tab.view.top > 0 {
            let thumb_h = (vb.h * shown).max(28.0).min(vb.h);
            let y = vb.y + (vb.h - thumb_h) * frac;
            let hot = matches!(self.hover, Hit::VBar) || matches!(tab.view.drag, Some(super::editor::DragMode::VScroll { .. }));
            let w = if hot { 8.0 } else { 5.0 };
            // match marks
            if let Some(f) = &tab.search.found {
                if self.find.open && !f.positions.is_empty() {
                    let key = (tab.doc.version, f.positions.len(), vb.y.to_bits(), vb.h.to_bits());
                    if tab.search.marks.as_ref().is_none_or(|m| m.0 != key) {
                        let len = tab.doc.len().max(1) as f64;
                        let mut last_y = -10.0f32;
                        let mut ys = Vec::new();
                        for &(s, _) in &f.positions {
                            let my = vb.y + (vb.h as f64 * s as f64 / len) as f32;
                            if my - last_y >= 2.0 {
                                ys.push(my);
                                last_y = my;
                            }
                        }
                        tab.search.marks = Some((key, ys));
                    }
                    for &my in tab.search.marks.as_ref().map_or(&[][..], |m| &m.1[..]) {
                        self.g.fill(Rect::new(vb.right() - 4.0, my, 3.0, 2.0), t.scroll_mark);
                    }
                }
            }
            self.g.fill_round(
                Rect::new(vb.right() - w - 3.0, y + 2.0, w, thumb_h - 4.0),
                w / 2.0,
                if hot { t.scroll_thumb_hover } else { t.scroll_thumb },
            );
        }
        if !self.style.wrap && tab.view.content_w > geom.text_w {
            let hb = geom.hbar();
            let total = tab.view.content_w + 40.0;
            let thumb_w = (hb.w * geom.text_w / total).max(28.0).min(hb.w);
            let max = (total - geom.text_w).max(1.0);
            let x = hb.x + (hb.w - thumb_w) * (tab.view.scroll_x / max).clamp(0.0, 1.0);
            let hot = matches!(self.hover, Hit::HBar);
            let h = if hot { 8.0 } else { 5.0 };
            self.g.fill_round(
                Rect::new(x + 2.0, hb.bottom() - h - 3.0, thumb_w - 4.0, h),
                h / 2.0,
                if hot { t.scroll_thumb_hover } else { t.scroll_thumb },
            );
        }
    }

    fn paint_status(&mut self) {
        let t = self.theme.clone();
        let r = self.r_status;
        self.g.fill(r, t.frame);
        self.g.hline(r.x, r.right(), r.y, false, t.border);
        let fonts_ui = self.fonts.ui.clone();
        let StatusTexts { mut pos, pos_short, mut items, counts, size } = self.status_items();
        let w = |s: &str| self.g.measure(s, &fonts_ui).0;
        let mut right = match &counts {
            Some(c) => format!("{c}  ·  {size}"),
            None => size.clone(),
        };
        let (mut pos_w, mut right_w) = (w(&pos), w(&right));
        let mut item_ws: Vec<f32> = items.iter().map(|(_, l)| w(l)).collect();
        // In a narrow window things give way, the least needed first: the counts, the selection's words and lines,
        // the indentation, the line endings, the language.
        let too_wide = |pos_w: f32, item_ws: &[f32], right_w: f32| {
            6.0 + pos_w + 40.0 + item_ws.iter().map(|w| w + 20.0).sum::<f32>() + right_w + 24.0 > r.w
        };
        if too_wide(pos_w, &item_ws, right_w) && counts.is_some() {
            right = size;
            right_w = w(&right);
        }
        if too_wide(pos_w, &item_ws, right_w) && pos_short != pos {
            pos = pos_short;
            pos_w = w(&pos);
        }
        for less in [StatusItem::Indent, StatusItem::Eol, StatusItem::Lang] {
            if too_wide(pos_w, &item_ws, right_w) {
                if let Some(k) = items.iter().position(|(i, _)| *i == less) {
                    items.remove(k);
                    item_ws.remove(k);
                }
            }
        }
        let pr = Rect::new(r.x + 6.0, r.y + 2.0, pos_w + 16.0, r.h - 4.0);
        // Right-aligned items, after the size (and the counts) at the end.
        let mut x = r.right() - 12.0;
        let mut rects = Vec::new();
        self.g.text(&right, &fonts_ui, Rect::new(x - right_w, r.y, right_w + 2.0, r.h), t.text_dim, Align::Left);
        x -= right_w + 12.0;
        for ((item, label), &w) in items.iter().zip(&item_ws).rev() {
            let br = Rect::new(x - w - 16.0, r.y + 2.0, w + 16.0, r.h - 4.0);
            if let Some(bg) = self.hot(Hit::Status(*item)) {
                self.g.fill_round(br, 4.0, bg);
            }
            let color = match item {
                StatusItem::Update => t.accent,
                StatusItem::Overtype => t.text,
                _ => t.text_dim,
            };
            self.g.text(label, &fonts_ui, br, color, Align::Center);
            rects.push((*item, br));
            x = br.x - 4.0;
        }
        // Left: position, then progress or a message.
        if let Some(bg) = self.hot(Hit::Status(StatusItem::Position)) {
            self.g.fill_round(pr, 4.0, bg);
        }
        self.g.text(&pos, &fonts_ui, pr, t.text_dim, Align::Center);
        rects.push((StatusItem::Position, pr));
        let msg_x = pr.right() + 16.0;
        let msg_w = (x - msg_x - 8.0).max(0.0);
        let (msg, bad) = match self.status_message() {
            Some(m) => m,
            None => (String::new(), false),
        };
        if !msg.is_empty() {
            self.g.text(&msg, &fonts_ui, Rect::new(msg_x, r.y, msg_w, r.h), if bad { t.error } else { t.text_dim }, Align::Left);
        }
        self.status_rects = rects;
    }

    /// What the status bar says.
    pub fn status_items(&mut self) -> StatusTexts {
        let counts = self.doc_counts_now();
        let sel_counts = self.selection_counts();
        let place = {
            let tab = &self.tabs[self.active];
            self.caret_place.get(&tab.doc, tab.view.sel.caret)
        };
        let indent = match self.indent_now() {
            Indent::Spaces(n) => format!("Spaces: {n}"),
            Indent::Tabs => format!("Tab size: {}", self.settings.tab_size),
        };
        let mut items: Vec<(StatusItem, String)> = Vec::new();
        let tab = &self.tabs[self.active];
        let doc = &tab.doc;
        if self.overtype {
            items.push((StatusItem::Overtype, "OVR".into()));
        }
        if self.settings.zoom != 1.0 {
            items.push((StatusItem::Zoom, format!("{:.0}%", self.settings.zoom * 100.0)));
        }
        if let UpdateState::Available(r) = &self.update {
            items.push((StatusItem::Update, format!("Update to {}", r.version)));
        }
        items.push((StatusItem::Indent, indent));
        items.push((StatusItem::Lang, tab.lang.label().to_string()));
        items.push((StatusItem::Eol, doc.eol.short().to_string()));
        items.push((StatusItem::Encoding, doc.encoding.label()));
        let counts = counts
            .filter(|c| c.chars > 0)
            .map(|c| format!("{}, {}", plural(c.words, "word", "words"), plural(c.chars, "character", "characters")));
        let (pos, pos_short) = position_text(tab, sel_counts, place);
        StatusTexts { pos, pos_short, items, counts, size: format_size(doc.len()) }
    }

    /// The active document's characters and words: counted at once when it's small, on another thread up to
    /// 64 MiB (meanwhile the last count of that tab), not at all for a bigger one or while it's still opening.
    pub fn doc_counts_now(&mut self) -> Option<Counts> {
        const AT_ONCE: u64 = 1 << 20;
        const MAX: u64 = 64 << 20;
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let (id, version, len) = (tab.id, tab.doc.version, tab.doc.len());
        let last = self.doc_counts.filter(|c| c.0 == id);
        if let Some((_, v, c)) = last {
            if v == version {
                return Some(c);
            }
        }
        if !tab.doc.is_ready() || tab.load_job.is_some() || len > MAX {
            return None;
        }
        if len <= AT_ONCE {
            let c = super::editor::count(&tab.doc.read(0, len));
            self.doc_counts = Some((id, version, c));
            return Some(c);
        }
        // Counting again after an edit waits until the typing stops for a moment (the count's snapshot of the text
        // ends the piece the typing goes into). One still running for an older text: the next starts after it.
        if last.is_some() {
            match self.count_wait {
                Some((v, at)) if v == version && at.elapsed() >= std::time::Duration::from_millis(700) => {}
                Some((v, _)) if v == version => return last.map(|c| c.2),
                _ => {
                    self.count_wait = Some((version, Instant::now()));
                    unsafe {
                        windows::Win32::UI::WindowsAndMessaging::SetTimer(self.hwnd, super::actions::TIMER_COUNT, 750, None);
                    }
                    return last.map(|c| c.2);
                }
            }
        }
        if self.count_job.is_none() {
            let snap = tab.doc.snapshot();
            let job = Job::spawn(len, notify, move |ctx| {
                let mut c = super::editor::Counter::default();
                let mut pos = 0;
                while pos < snap.len() {
                    if ctx.cancelled() {
                        return None;
                    }
                    let end = (pos + (1 << 20)).min(snap.len());
                    snap.chunks(pos, end, &mut |b| {
                        c.feed(b);
                        true
                    });
                    pos = end;
                    ctx.set(pos);
                }
                Some(c.finish())
            });
            self.count_job = Some((job, id, version));
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(self.hwnd, super::actions::TIMER_JOBS, 100, None);
            }
        }
        last.map(|c| c.2)
    }

    /// Picks up a finished background count; returns whether one is still running.
    pub fn poll_counts(&mut self) -> bool {
        let Some((job, _, _)) = self.count_job.as_mut() else { return false };
        let Some(r) = job.take() else { return true };
        let (_, id, version) = self.count_job.take().unwrap();
        if let Some(c) = r {
            self.doc_counts = Some((id, version, c));
        }
        false
    }

    /// Characters and words of the selection (up to 4 MiB of it), counted once per selection.
    fn selection_counts(&mut self) -> Option<Counts> {
        let tab = &self.tabs[self.active];
        let sel = tab.view.sel;
        if sel.is_empty() || sel.end() - sel.start() > 4 << 20 {
            return None;
        }
        let key = (tab.doc.version, sel.start(), sel.end());
        if let Some((k, c)) = self.sel_counts {
            if k == key {
                return Some(c);
            }
        }
        let c = super::editor::count(&tab.doc.read(sel.start(), sel.end()));
        self.sel_counts = Some((key, c));
        Some(c)
    }

    /// Progress of background work, or a recent message.
    pub fn status_message(&self) -> Option<(String, bool)> {
        let tab = self.tab();
        if let Some(s) = &tab.save {
            let then = if s.close_after { " (closes when done)" } else { "" };
            return Some((format!("Saving… {:.0}%{then}  (Esc to cancel)", s.job.fraction() * 100.0), false));
        }
        if let Some(task) = &tab.task {
            let what = match task.kind {
                TaskKind::Format(f) => format!("Formatting {}", f.name()),
                TaskKind::Minify(f) => format!("Minifying {}", f.name()),
                TaskKind::Validate(f) => format!("Checking {}", f.name()),
                TaskKind::ReplaceAll => "Replacing".into(),
                TaskKind::Eol(_) => "Converting line endings".into(),
                TaskKind::Lines(LineOp::SortAsc | LineOp::SortDesc) => "Sorting lines".into(),
                TaskKind::Lines(_) => "Cleaning up lines".into(),
            };
            return Some((format!("{what}… {:.0}%  (Esc to cancel)", task.job.fraction() * 100.0), false));
        }
        // A recent message comes before progress below: it often says why something didn't happen ("Still reading
        // the file's lines…" while that runs).
        if let Some((m, at, bad)) = &self.flash {
            if at.elapsed().as_secs() < 6 {
                return Some((m.clone(), *bad));
            }
        }
        if let Some(j) = &tab.index_job {
            let then = match tab.goto {
                Some(Goto::Line(l, _)) => format!(" (then to line {})", group(l)),
                _ => String::new(),
            };
            return Some((format!("Reading lines… {:.0}%{then}", j.fraction() * 100.0), false));
        }
        if let UpdateState::Downloading { release, job } = &self.update {
            return Some((format!("Downloading Slate {}… {:.0}%", release.version, job.fraction() * 100.0), false));
        }
        if let Some(j) = &tab.search.job {
            if self.find.open {
                return Some((format!("Searching… {:.0}%", j.fraction() * 100.0), false));
            }
        }
        if tab.doc.read_errors() > 0 {
            return Some(("Part of the file couldn't be read (it may have changed on disk).".into(), true));
        }
        None
    }
}

/// What the status bar says (`App::status_items`).
pub struct StatusTexts {
    /// The position with what's selected, and the same without the selection's words and lines (for a narrow window).
    pub pos: String,
    pub pos_short: String,
    /// The items on the right, left to right.
    pub items: Vec<(StatusItem, String)>,
    /// The document's word and character counts (when known), and its size.
    pub counts: Option<String>,
    pub size: String,
}

pub fn tab_progress(tab: &Tab) -> Option<f32> {
    if let Some(f) = tab.opening() {
        return Some(f);
    }
    if let Some(s) = &tab.save {
        return Some(s.job.fraction());
    }
    if let Some(t) = &tab.task {
        return Some(t.job.fraction());
    }
    if let Some(j) = &tab.index_job {
        return Some(j.fraction());
    }
    None
}

pub fn format_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} bytes")
    } else if f < K * K {
        format!("{:.1} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / K / K)
    } else {
        format!("{:.2} GB", f / K / K / K)
    }
}

/// Where the caret is in its line, for the status bar: its line, the line's start and the column (in characters,
/// from 1; None more than 4 MiB into the line), worked out once per caret place and text, as in a long line the
/// column means reading megabytes (which every frame would cost a millisecond or two).
#[derive(Default)]
pub struct CaretPlace {
    key: Option<(u64, u64, Option<u64>)>,
    start: u64,
    col: Option<u64>,
}

impl CaretPlace {
    pub fn get(&mut self, doc: &Document, caret: u64) -> (Option<u64>, u64, Option<u64>) {
        // (the line is unknown while that part of a big file isn't indexed yet)
        let line = doc.line_of(caret);
        let key = (doc.version, caret, line);
        if self.key != Some(key) {
            // Unknown line: finding the line start could mean reading hundreds of MB.
            self.start = if line.is_some() { doc.line_start_of(caret) } else { caret };
            self.col = (line.is_some() && caret - self.start <= 4 << 20)
                .then(|| bytecount::num_chars(&doc.read(self.start, caret)) as u64 + 1);
            self.key = Some(key);
        }
        (line, self.start, self.col)
    }
}

/// The caret's place ("Ln 3, Col 9") with what's selected, in full and without the selection's words and lines.
fn position_text(
    tab: &Tab,
    sel_counts: Option<Counts>,
    (line, ls, col): (Option<u64>, u64, Option<u64>),
) -> (String, String) {
    let doc = &tab.doc;
    let sel = tab.view.sel;
    let caret = sel.caret;
    let s = match (line, col) {
        (Some(l), Some(c)) => format!("Ln {}, Col {}", group(l + 1), group(c)),
        (Some(l), None) => format!("Ln {}, byte {}", group(l + 1), group(caret - ls + 1)),
        (None, _) => format!("Byte {}", group(caret + 1)),
    };
    if sel.is_empty() {
        return (s.clone(), s);
    }
    match sel_counts {
        Some(c) => {
            // (lines: from the line of the start to that of the end)
            let lines = match (doc.line_of(sel.start()), doc.line_of(sel.end())) {
                (Some(a), Some(b)) => b - a + 1,
                _ => 1,
            };
            let words = plural(c.words, "word", "words");
            let chars = group(c.chars);
            let full = if lines > 1 {
                format!("{s}  ({chars} selected, {words}, {} lines)", group(lines))
            } else {
                format!("{s}  ({chars} selected, {words})")
            };
            (full, format!("{s}  ({chars} selected)"))
        }
        None => {
            let s = format!("{s}  ({} selected)", format_size(sel.end() - sel.start()));
            (s.clone(), s)
        }
    }
}

pub fn make_theme(mode: ThemeMode) -> Theme {
    if high_contrast_on() {
        return Theme::high_contrast();
    }
    let accent = system_accent();
    let dark = match mode {
        ThemeMode::System => system_prefers_dark(),
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
    };
    if dark { Theme::dark(accent) } else { Theme::light(accent) }
}

fn make_fonts(g: &Gfx) -> UiFonts {
    let family = pick_family(&g.dw, &["Segoe UI Variable Text", "Segoe UI"]);
    let icon_family = pick_family(&g.dw, &["Segoe Fluent Icons", "Segoe MDL2 Assets"]);
    UiFonts {
        ui: g.ui_format(&family, metrics::UI_FONT_SIZE + 0.5, DWRITE_FONT_WEIGHT_NORMAL),
        ui_semibold: g.ui_format(&family, metrics::UI_FONT_SIZE + 0.5, DWRITE_FONT_WEIGHT_SEMI_BOLD),
        icons: g.ui_format(&icon_family, 14.0, DWRITE_FONT_WEIGHT_NORMAL),
        icons_small: g.ui_format(&icon_family, 10.0, DWRITE_FONT_WEIGHT_NORMAL),
        family,
    }
}

pub fn make_style(g: &Gfx, s: &Settings, generation: u64) -> Style {
    let size = s.font_size * 96.0 / 72.0 * s.zoom;
    let family = if font_info(&g.dw, &s.font, size).is_some() { s.font.clone() } else { "Consolas".to_string() };
    let info = font_info(&g.dw, &family, size)
        .unwrap_or(super::gfx::FontInfo { line_height: (size * 1.35).round(), baseline: (size * 1.05).round(), monospace: true, bold: true });
    let make = |wrap: bool| {
        let f = g.format(&family, size, DWRITE_FONT_WEIGHT_NORMAL);
        unsafe {
            let _ = f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            let _ = f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, info.line_height, info.baseline);
        }
        f
    };
    let format_wrap = make(true);
    let format_nowrap = make(false);
    let (char_w, _) = g.measure("0000000000", &format_nowrap);
    let char_w = char_w / 10.0;
    unsafe {
        let _ = format_wrap.SetIncrementalTabStop(char_w * s.tab_size as f32);
        let _ = format_nowrap.SetIncrementalTabStop(char_w * s.tab_size as f32);
    }
    Style {
        format_wrap,
        format_nowrap,
        row_h: info.line_height,
        real_bold: info.bold,
        char_w,
        digit_w: char_w,
        wrap: s.wrap,
        tab_size: s.tab_size,
        use_spaces: s.use_spaces,
        line_numbers: s.line_numbers,
        show_whitespace: s.show_whitespace,
        generation,
    }
}

pub fn white() -> u32 {
    rgb(0xFFFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{EditKind, Sel};

    #[test]
    fn the_status_bar_column_follows_the_caret_and_the_text() {
        // (columns count characters: ç and € take 2 and 3 bytes)
        let mut d = Document::from_text("ab\nçd€x\n".as_bytes());
        let mut p = CaretPlace::default();
        assert_eq!(p.get(&d, 9), (Some(1), 3, Some(4)));
        assert_eq!(p.get(&d, 5), (Some(1), 3, Some(2)));
        // the same place in a changed text: worked out again
        d.begin(EditKind::Other, Sel::at(3));
        d.insert(3, b"12");
        d.end(Sel::at(5));
        assert_eq!(p.get(&d, 5), (Some(1), 3, Some(3)));
        assert_eq!(p.get(&d, 0), (Some(0), 0, Some(1)));
    }
}
