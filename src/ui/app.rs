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
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::core::document::Document;
use crate::core::io::{SaveError, Saved};
use crate::core::job::{Job, Notify};
use crate::core::lines::LineOp;
use crate::core::search::Found;
use crate::core::source::Source;
use crate::core::text::{Encoding, Eol};

use super::commands::{Cmd, MENU_TITLES};
use super::update::Release;
use super::editor::{Ctx, Geom, Style, View};
use super::findbar::{self, FindBar};
use super::gfx::{Align, Gfx, Rect, font_info, rgb};
use super::highlight::Lang;
use super::settings::{Settings, ThemeMode};
use super::theme::{Theme, metrics, system_accent, system_prefers_dark};
use super::win;

pub type Cell = Rc<RefCell<App>>;

pub struct SaveTask {
    pub job: Job<Result<Saved, SaveError>>,
    pub state: u64,
    pub version: u64,
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
}

pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    pub actions: Vec<(String, NoticeAction)>,
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
    pub load_job: Option<Job<std::io::Result<Document>>>,
    pub reload: bool,
    pub save: Option<SaveTask>,
    pub task: Option<Task>,
    pub search: Search,
    /// A find next/previous running in the background (big documents): result and the doc version it's for.
    pub find_job: Option<(Job<Option<(u64, u64, bool)>>, u64)>,
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
    /// A save stopped because ANSI can't hold some characters: (path, close after, closing the window), for the
    /// question that follows (`Deferred::AskLossy`).
    pub ask_lossy: Option<(PathBuf, bool, bool)>,
    /// The document's path and its canonical form, worked out once (for "is this file open already?").
    pub canon: Option<(PathBuf, Option<PathBuf>)>,
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
            reload: false,
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
            ask_lossy: None,
            canon: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.doc.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
            None if self.untitled > 1 => format!("Untitled {}", self.untitled),
            None => "Untitled".into(),
        }
    }

    pub fn busy(&self) -> bool {
        self.load_job.is_some() || self.save.is_some() || self.task.is_some()
    }

    pub fn is_blank(&self) -> bool {
        self.doc.path.is_none() && self.doc.is_empty() && !self.doc.is_dirty() && self.load_job.is_none()
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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusItem {
    Position,
    Zoom,
    Eol,
    Encoding,
    Lang,
    Update,
}

/// Where updating Slate is at (see `update.rs`).
pub enum UpdateState {
    Idle,
    /// Asking GitHub; `manual`: from the Help menu (so say what came out of it).
    Checking { manual: bool, job: Job<Result<Release, String>> },
    Available(Release),
    Downloading { release: Release, job: Job<Result<std::path::PathBuf, String>> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    None,
    Tab(usize),
    TabClose(usize),
    NewTab,
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
    pub tab_rects: Vec<(Rect, Rect)>,
    pub newtab_rect: Rect,
    pub menu_rects: Vec<Rect>,
    pub theme_rect: Rect,
    pub status_rects: Vec<(StatusItem, Rect)>,
    pub notice_rects: Vec<Rect>,
    pub tab_scroll: f32,
    pub hover: Hit,
    pub down: Hit,
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
    pub mouse_tracking: bool,
    /// `g.generation` the cached layouts were made for.
    pub gfx_generation: u64,
    /// Looking at the open files on disk (a network drive that went away can take long to answer).
    pub disk_job: Option<Job<Vec<DiskCheck>>>,
}

pub const ZOOM_STEPS: [f32; 15] = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

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
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
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
            tab_rects: Vec::new(),
            newtab_rect: Rect::default(),
            menu_rects: Vec::new(),
            theme_rect: Rect::default(),
            status_rects: Vec::new(),
            notice_rects: Vec::new(),
            tab_scroll: 0.0,
            hover: Hit::None,
            down: Hit::None,
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
            mouse_tracking: false,
            gfx_generation: 0,
            disk_job: None,
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
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    pub fn flash(&mut self, msg: impl Into<String>, bad: bool) {
        self.flash = Some((msg.into(), Instant::now(), bad));
        self.invalidate();
    }

    pub fn new_tab_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Re-reads the theme (settings or Windows changed).
    pub fn apply_theme(&mut self) {
        self.theme = make_theme(self.settings.theme);
        win::style_title_bar(self.hwnd, self.theme.dark, self.theme.frame, self.theme.text);
        win::set_menu_dark(self.theme.dark);
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
        self.dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
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
        let mut y = 0.0;
        self.r_tabs = Rect::new(0.0, y, w, metrics::TAB_BAR_H);
        y += metrics::TAB_BAR_H;
        self.r_menu = Rect::new(0.0, y, w, metrics::MENU_BAR_H);
        y += metrics::MENU_BAR_H;
        let fh = self.find.height();
        self.r_find = Rect::new(0.0, y, w, fh);
        y += fh;
        let nh = if self.tabs.get(self.active).is_some_and(|t| t.notice.is_some()) { metrics::NOTICE_H } else { 0.0 };
        self.r_notice = Rect::new(0.0, y, w, nh);
        y += nh;
        let json = self.tabs.get(self.active).is_some_and(|t| t.lang == Lang::Json);
        let ph = if json && self.settings.path_bar { super::structure::PATH_H } else { 0.0 };
        self.r_path = Rect::new(0.0, y, w, ph);
        y += ph;
        let sh = metrics::STATUS_H;
        self.r_status = Rect::new(0.0, (h - sh).max(y), w, sh);
        let body_h = (h - sh - y).max(0.0);
        if json && self.settings.structure_panel {
            let pw = self.settings.structure_width.clamp(200.0, (w * 0.6).max(200.0));
            self.r_struct = Rect::new((w - pw).max(0.0), y, pw.min(w), body_h);
            self.r_edit = Rect::new(0.0, y, (w - pw).max(0.0), body_h);
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
        let avail = (bar.w - left - new_w - 16.0).max(metrics::TAB_MIN_W);
        let n = self.tabs.len().max(1) as f32;
        let tw = (avail / n).clamp(metrics::TAB_MIN_W, metrics::TAB_MAX_W);
        let total = tw * self.tabs.len() as f32;
        let max_scroll = (total - avail).max(0.0);
        self.tab_scroll = self.tab_scroll.clamp(0.0, max_scroll);
        let top = bar.y + 6.0;
        let h = bar.h - 6.0;
        for i in 0..self.tabs.len() {
            let x = bar.x + left + i as f32 * tw - self.tab_scroll;
            let r = Rect::new(x, top, tw, h);
            let c = Rect::new(r.right() - 32.0, top + (h - 24.0) / 2.0, 24.0, 24.0);
            self.tab_rects.push((r, c));
        }
        let nx = (bar.x + left + total - self.tab_scroll).min(bar.x + left + avail) + 4.0;
        self.newtab_rect = Rect::new(nx, top + (h - 28.0) / 2.0, 28.0, 28.0);
    }

    /// Scrolls the tab strip so the active tab is visible.
    pub fn reveal_active_tab(&mut self) {
        self.layout_tabs();
        if let Some((r, _)) = self.tab_rects.get(self.active) {
            let bar = self.r_tabs;
            let left = bar.x + 8.0;
            let right = bar.right() - 52.0;
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
        self.theme_rect = Rect::new(self.r_menu.right() - 44.0, self.r_menu.y + 3.0, 34.0, self.r_menu.h - 6.0);
    }

    pub fn editor_geom(&self) -> Geom {
        View::geometry(&self.tab().doc, &self.style, self.r_edit)
    }

    // ---- hit testing ----

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        if self.r_tabs.contains(x, y) {
            for (i, (r, c)) in self.tab_rects.iter().enumerate() {
                if r.contains(x, y) && x >= self.r_tabs.x && x < self.r_tabs.right() - 44.0 {
                    if c.contains(x, y) {
                        return Hit::TabClose(i);
                    }
                    return Hit::Tab(i);
                }
            }
            if self.newtab_rect.contains(x, y) {
                return Hit::NewTab;
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
        let f = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        if !self.g.offscreen {
            // Asked here rather than tracked: focus moved while the app was busy never reaches WM_SETFOCUS.
            self.focused = f == self.hwnd;
        }
        let focused_edit = if self.find.is_edit(f) { Some(f) } else { None };
        let hover_part = if let Hit::Find(p) = self.hover { Some(p) } else { None };
        self.find.paint(&self.g, &self.theme, self.r_find, &self.fonts.ui, &self.fonts.icons_small, hover_part, focused_edit);
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
    }

    fn paint_tabs(&mut self) {
        let t = self.theme.clone();
        let g = &self.g;
        g.fill(self.r_tabs, t.frame);
        let clip = Rect::new(self.r_tabs.x, self.r_tabs.y, self.r_tabs.w - 44.0, self.r_tabs.h);
        g.push_clip(clip);
        for (i, (r, c)) in self.tab_rects.iter().enumerate() {
            if r.right() < clip.x || r.x > clip.right() {
                continue;
            }
            let tab = &self.tabs[i];
            let active = i == self.active;
            let hovered = matches!(self.hover, Hit::Tab(j) | Hit::TabClose(j) if j == i);
            if active {
                // Rounded top corners; the bottom merges into the menu bar.
                g.fill_round(Rect::new(r.x, r.y, r.w, r.h + 10.0), 7.0, t.surface);
            } else if hovered {
                g.fill_round(Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 6.0), 6.0, t.hover);
            } else if i + 1 != self.active && i + 1 < self.tabs.len() {
                // separator between inactive tabs
                g.line(r.right() - 0.5, r.y + 10.0, r.right() - 0.5, r.bottom() - 10.0, t.border, 1.0);
            }
            let busy = tab.busy() || !tab.doc.is_ready();
            let title = tab.title();
            let text_r = Rect::new(r.x + 14.0, r.y, (c.x - r.x - 18.0).max(10.0), r.h - 2.0);
            let color = if active { t.text } else { t.text_dim };
            g.text(&title, &self.fonts.ui, text_r, color, Align::Left);
            let show_close = hovered || active;
            if show_close {
                if self.hover == Hit::TabClose(i) {
                    g.fill_round(*c, 4.0, t.hover);
                }
                g.text("\u{E711}", &self.fonts.icons_small, *c, t.text_dim, Align::Center);
            } else if tab.doc.is_dirty() {
                let d = 8.0;
                g.fill_round(Rect::new(c.x + (c.w - d) / 2.0, c.y + (c.h - d) / 2.0, d, d), d / 2.0, t.text_dim);
            }
            if active && tab.doc.is_dirty() && show_close && self.hover != Hit::TabClose(i) {
                // dirty dot shown next to the close button on the active tab
                let d = 6.0;
                g.fill_round(Rect::new(c.x - 10.0, c.y + (c.h - d) / 2.0, d, d), d / 2.0, t.text_dim);
            }
            if busy {
                let pct = tab_progress(tab);
                let bw = (r.w - 28.0) * pct.unwrap_or(0.35);
                g.fill(Rect::new(r.x + 14.0, r.bottom() - 4.0, bw.max(4.0), 2.0), t.accent);
            }
        }
        g.pop_clip();
        let nr = self.newtab_rect;
        if self.hover == Hit::NewTab {
            g.fill_round(nr, 5.0, t.hover);
        }
        g.text("\u{E710}", &self.fonts.icons_small, nr, t.text_dim, Align::Center);
    }

    fn paint_menu(&mut self) {
        let t = &self.theme;
        let g = &self.g;
        g.fill(self.r_menu, t.surface);
        for (i, r) in self.menu_rects.iter().enumerate() {
            if self.menu_open == Some(i) {
                g.fill_round(*r, 5.0, t.pressed);
            } else if self.hover == Hit::Menu(i) {
                g.fill_round(*r, 5.0, t.hover);
            }
            g.text(MENU_TITLES[i], &self.fonts.ui, *r, t.text, Align::Center);
        }
        // Light/dark switch: a sun in dark mode, a moon in light mode.
        if self.hover == Hit::ThemeToggle {
            g.fill_round(self.theme_rect, 5.0, t.hover);
        }
        let glyph = if t.dark { "\u{E706}" } else { "\u{E708}" };
        g.text(glyph, &self.fonts.icons, self.theme_rect, t.text_dim, Align::Center);
        if self.find.open || self.tab().notice.is_some() {
            return;
        }
        g.line(0.0, self.r_menu.bottom() - 0.5, self.r_menu.right(), self.r_menu.bottom() - 0.5, t.border, 1.0);
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
        g.line(r.x, r.bottom() - 0.5, r.right(), r.bottom() - 0.5, t.border, 1.0);
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
            tab.structure.paint_panel(&self.g, &t, &ui, &bold, &icons, r_struct, row_hover, hover == Hit::StructClose);
            if hover == Hit::StructSplitter || self.split_drag.is_some() {
                self.g.fill(Rect::new(r_struct.x, r_struct.y, 2.0, r_struct.h), t.accent);
            }
        }
    }

    /// Keeps the JSON path (and the panel's rows) up to date with the caret.
    pub fn update_structure(&mut self) {
        let json = self.tabs.get(self.active).is_some_and(|t| t.lang == Lang::Json);
        if !json || (self.r_path.h <= 0.0 && self.r_struct.w <= 0.0) {
            return;
        }
        let panel = self.r_struct.w > 0.0;
        let notify = self.notify.clone();
        let tab = &mut self.tabs[self.active];
        let caret = tab.view.sel.caret;
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
        tab.view.paint(&cx, focused && self.focused, caret_on, &matches);
        // Loading / converting overlay.
        if let Some(job) = &tab.load_job {
            let msg = format!("Opening… {:.0}%", job.fraction() * 100.0);
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
                    let len = tab.doc.len().max(1) as f64;
                    let mut last_y = -10.0f32;
                    for &(s, _) in &f.positions {
                        let my = vb.y + (vb.h as f64 * s as f64 / len) as f32;
                        if my - last_y >= 2.0 {
                            self.g.fill(Rect::new(vb.right() - 4.0, my, 3.0, 2.0), t.scroll_mark);
                            last_y = my;
                        }
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
        self.g.line(r.x, r.y + 0.5, r.right(), r.y + 0.5, t.border, 1.0);
        let fonts_ui = self.fonts.ui.clone();
        let mut items: Vec<(StatusItem, String)> = Vec::new();
        let tab = &self.tabs[self.active];
        let doc = &tab.doc;
        if self.settings.zoom != 1.0 {
            items.push((StatusItem::Zoom, format!("{:.0}%", self.settings.zoom * 100.0)));
        }
        if let UpdateState::Available(r) = &self.update {
            items.push((StatusItem::Update, format!("Update to {}", r.version)));
        }
        items.push((StatusItem::Lang, tab.lang.label().to_string()));
        items.push((StatusItem::Eol, doc.eol.short().to_string()));
        items.push((StatusItem::Encoding, doc.encoding.label()));
        let size = format_size(doc.len());
        // Right-aligned items.
        let mut x = r.right() - 12.0;
        let mut rects = Vec::new();
        let (sw, _) = self.g.measure(&size, &fonts_ui);
        self.g.text(&size, &fonts_ui, Rect::new(x - sw, r.y, sw + 2.0, r.h), t.text_dim, Align::Left);
        x -= sw + 12.0;
        for (item, label) in items.iter().rev() {
            let (w, _) = self.g.measure(label, &fonts_ui);
            let br = Rect::new(x - w - 16.0, r.y + 2.0, w + 16.0, r.h - 4.0);
            if self.hover == Hit::Status(*item) {
                self.g.fill_round(br, 4.0, t.hover);
            }
            let color = if *item == StatusItem::Update { t.accent } else { t.text_dim };
            self.g.text(label, &fonts_ui, br, color, Align::Center);
            rects.push((*item, br));
            x = br.x - 4.0;
        }
        // Left: position, then progress or a message.
        let pos = position_text(tab);
        let (pw, _) = self.g.measure(&pos, &fonts_ui);
        let pr = Rect::new(r.x + 6.0, r.y + 2.0, pw + 16.0, r.h - 4.0);
        if self.hover == Hit::Status(StatusItem::Position) {
            self.g.fill_round(pr, 4.0, t.hover);
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

    /// Progress of background work, or a recent message.
    pub fn status_message(&self) -> Option<(String, bool)> {
        let tab = self.tab();
        if let Some(s) = &tab.save {
            let then = if s.close_after { " (closes when done)" } else { "" };
            return Some((format!("Saving… {:.0}%{then}", s.job.fraction() * 100.0), false));
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
        if let Some(j) = &tab.index_job {
            return Some((format!("Reading lines… {:.0}%", j.fraction() * 100.0), false));
        }
        if let UpdateState::Downloading { release, job } = &self.update {
            return Some((format!("Downloading Slate {}… {:.0}%", release.version, job.fraction() * 100.0), false));
        }
        if let Some(j) = &tab.search.job {
            if self.find.open {
                return Some((format!("Searching… {:.0}%", j.fraction() * 100.0), false));
            }
        }
        if let Some((m, at, bad)) = &self.flash {
            if at.elapsed().as_secs() < 6 {
                return Some((m.clone(), *bad));
            }
        }
        if tab.doc.read_errors() > 0 {
            return Some(("Part of the file couldn't be read (it may have changed on disk).".into(), true));
        }
        None
    }
}

pub fn tab_progress(tab: &Tab) -> Option<f32> {
    if let Some(j) = &tab.load_job {
        return Some(j.fraction());
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

pub fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn position_text(tab: &Tab) -> String {
    let doc = &tab.doc;
    let sel = tab.view.sel;
    let caret = sel.caret;
    let line = doc.line_of(caret);
    // Unknown line (that part isn't indexed yet): finding the line start could mean reading hundreds of MB.
    let ls = if line.is_some() { doc.line_start_of(caret) } else { caret };
    let col = if line.is_some() && caret - ls <= 4 << 20 {
        let b = doc.read(ls, caret);
        Some(bytecount::num_chars(&b) as u64 + 1)
    } else {
        None
    };
    let mut s = match (line, col) {
        (Some(l), Some(c)) => format!("Ln {}, Col {}", group(l + 1), group(c)),
        (Some(l), None) => format!("Ln {}, byte {}", group(l + 1), group(caret - ls + 1)),
        (None, _) => format!("Byte {}", group(caret + 1)),
    };
    if !sel.is_empty() {
        let n = sel.end() - sel.start();
        if n <= 4 << 20 {
            let b = doc.read(sel.start(), sel.end());
            let chars = bytecount::num_chars(&b) as u64;
            let lines = bytecount::count(&b, b'\n') as u64;
            if lines > 0 {
                s.push_str(&format!("  ({} selected, {} lines)", group(chars), group(lines + 1)));
            } else {
                s.push_str(&format!("  ({} selected)", group(chars)));
            }
        } else {
            s.push_str(&format!("  ({} selected)", format_size(n)));
        }
    }
    s
}

pub fn make_theme(mode: ThemeMode) -> Theme {
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
        .unwrap_or(super::gfx::FontInfo { line_height: (size * 1.35).round(), baseline: (size * 1.05).round(), monospace: true });
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
        char_w,
        digit_w: char_w,
        wrap: s.wrap,
        tab_size: s.tab_size,
        use_spaces: s.use_spaces,
        line_numbers: s.line_numbers,
        generation,
    }
}

pub fn white() -> u32 {
    rgb(0xFFFFFF)
}
