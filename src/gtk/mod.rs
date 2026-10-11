//! Slate's window on Linux (GTK 4): a menu bar, the tab strip, the find bar, the text and the status bar, around
//! the same engine, colors and editing operations as on Windows. One Slate per user session comes from GTK
//! itself: a second start hands its files to the running one (D-Bus). Background work notifies the window through
//! GLib's main loop.

mod app;
mod chrome;
mod session;
mod testmode;
mod view;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{cairo, gdk, gio, glib, pango};

use crate::core::document::Sel;
use crate::core::job::Notify;
use crate::core::lines::{CaseOp, LineOp};
use crate::core::text::Eol;
use crate::highlight::Lang;
use crate::settings::{Settings, ThemeMode};

use app::{App, Ask, Cmd};
use view::{Geom, set_color};

pub const APP_ID: &str = "io.github.jamesccupps.Slate";

/// `SLATE_TIMING=1`: how long painting and keys take, on stderr (for measuring).
struct Timing(&'static str, Instant);

impl Drop for Timing {
    fn drop(&mut self) {
        thread_local! {
            static ON: bool = std::env::var_os("SLATE_TIMING").is_some();
        }
        if ON.with(|o| *o) {
            eprintln!("{} {:.2} ms", self.0, self.1.elapsed().as_secs_f64() * 1000.0);
        }
    }
}
/// Space between the gutter (or the window's edge) and the text.
const PAD: f64 = 8.0;
const BLINK_MS: u64 = 530;
/// The caret stops blinking (shown) this long after the last key or click.
const BLINK_STOP: Duration = Duration::from_secs(10);

pub struct Ui {
    pub app: RefCell<App>,
    pub gtk_app: gtk4::Application,
    pub window: gtk4::ApplicationWindow,
    tabs: gtk4::DrawingArea,
    text: gtk4::DrawingArea,
    status: gtk4::DrawingArea,
    vbar: gtk4::Scrollbar,
    hbar: gtk4::Scrollbar,
    find_box: gtk4::Box,
    find_entry: gtk4::Entry,
    replace_row: gtk4::Box,
    replace_entry: gtk4::Entry,
    find_count: gtk4::Label,
    case_btn: gtk4::ToggleButton,
    word_btn: gtk4::ToggleButton,
    regex_btn: gtk4::ToggleButton,
    notice: gtk4::Box,
    notice_label: gtk4::Label,
    /// The notice's Save as… button (after a save that failed).
    notice_save_as: gtk4::Button,
    status_menu: gtk4::PopoverMenu,
    im: gtk4::IMMulticontext,
    css: gtk4::CssProvider,
    strip: RefCell<chrome::TabStrip>,
    first_tab: Cell<usize>,
    hover: Cell<Option<chrome::TabPart>>,
    status_items: RefCell<Vec<(usize, f64, f64)>>,
    /// Set while the window sets its own scrollbars and boxes (their signals then aren't the user's).
    syncing: Cell<bool>,
    scroll_acc: Cell<f64>,
    drag_origin: Cell<(f64, f64)>,
    last_input: Cell<Instant>,
    polling: Cell<bool>,
    /// Whether the window's own widgets (menus, boxes) are styled dark.
    css_dark: Cell<Option<bool>>,
    /// The window may close now (the session is written).
    closing: Cell<bool>,
    /// The text was drawn once (the tabs from the session are shown: `session::restored`).
    painted: Cell<bool>,
    /// Logging out waits while there's unsaved work the session can't keep (the session manager's cookie).
    inhibited: Cell<Option<u32>>,
    /// File → Open recent, and the files it shows.
    recent_menu: gio::Menu,
    recent_shown: RefCell<Option<Vec<PathBuf>>>,
    /// Slate opens text files when they're double-clicked (Help shows Stop opening files with Slate… then).
    is_default: Cell<bool>,
    pub test: Option<Rc<testmode::Test>>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

/// Set by SIGTERM, SIGHUP or SIGINT (the system stopping Slate): it writes the session and ends.
static STOPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn get_ui() -> Option<Rc<Ui>> {
    UI.with(|u| u.borrow().clone())
}

/// Background jobs wake the window through the main loop when they finish.
fn notifier() -> Notify {
    Arc::new(|| {
        glib::idle_add_once(|| {
            if let Some(ui) = get_ui() {
                ui.poll();
            }
        });
    })
}

/// Adds a line to crash.log in the data folder.
fn log_crash(what: &str) {
    let dir = crate::settings::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let now = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")) {
        let _ = writeln!(f, "[{now}] Slate {}: {what}", env!("CARGO_PKG_VERSION"));
    }
}

/// What to hand GApplication for a file named on the command line: an absolute path (GIO reads a relative
/// `notes.txt:120` as a link with the scheme "notes.txt" and drops it), and a `file://` link for a name that isn't
/// UTF-8 (GTK only takes text). Anything else (an option, a link) as it is.
fn open_arg(a: std::ffi::OsString, cwd: Option<&std::path::Path>) -> String {
    use std::os::unix::ffi::OsStrExt;
    let bytes = a.as_bytes();
    let is_link = bytes.windows(3).position(|w| w == b"://").is_some_and(|k| k > 0 && bytes[..k].iter().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(b)));
    if bytes.first() == Some(&b'-') || is_link {
        return a.to_string_lossy().into_owned();
    }
    let p = PathBuf::from(&a);
    let abs = match cwd {
        Some(d) if p.is_relative() => d.join(&p),
        _ => p,
    };
    match abs.to_str() {
        Some(s) => s.to_string(),
        None => {
            let mut link = String::from("file://");
            for &b in abs.as_os_str().as_bytes() {
                if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
                    link.push(b as char);
                } else {
                    link.push_str(&format!("%{b:02X}"));
                }
            }
            link
        }
    }
}

pub fn run(args: Vec<std::ffi::OsString>) -> i32 {
    if args.first().is_some_and(|a| a == "--test") {
        let args: Vec<String> = args[1..].iter().map(|a| a.to_string_lossy().into_owned()).collect();
        return testmode::run(&args);
    }
    std::panic::set_hook(Box::new(|info| log_crash(&info.to_string())));
    let application = gtk4::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    // (logging out: the session manager asks, `query-end`)
    application.set_register_session(true);
    application.connect_activate(|a| {
        let ui = window(a, None);
        ui.window.present();
    });
    application.connect_open(|a, files, _| {
        let ui = window(a, None);
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        ui.with(|app| app.open_paths(&paths));
        ui.window.present();
    });
    application.connect_query_end(|_| {
        if let Some(ui) = get_ui() {
            ui.ending();
        }
    });
    let cwd = std::env::current_dir().ok();
    let argv: Vec<String> = std::iter::once("slate".to_string()).chain(args.into_iter().map(|a| open_arg(a, cwd.as_deref()))).collect();
    if application.run_with_args(&argv) == glib::ExitCode::SUCCESS { 0 } else { 1 }
}

/// GTK doesn't keep a native dialog alive while it's shown: dropped when the function showing it returns, its file
/// chooser was taken down at once (GTK 4.8 then said "The folder contents could not be displayed"). The response
/// handler holds this reference and lets it go once the dialog is answered.
#[allow(deprecated)]
fn keep_until_answered(chooser: &gtk4::FileChooserNative) -> RefCell<Option<gtk4::FileChooserNative>> {
    RefCell::new(Some(chooser.clone()))
}

/// Makes Slate the default app for the file types its desktop file lists (text/plain, application/json...): GIO
/// writes them to the user's `~/.config/mimeapps.list`, which file managers read. Linux lets an app do this itself;
/// on Windows the user has to pick Slate in Default apps.
fn make_default() -> Result<usize, String> {
    let id = format!("{APP_ID}.desktop");
    let Some(info) = gio::AppInfo::all().into_iter().find(|i| i.id().is_some_and(|s| s == id)) else {
        return Err("Slate isn't installed from its package (the .deb), so the system doesn't know it yet.".into());
    };
    let types = info.supported_types();
    let mut done = 0;
    for t in &types {
        if info.set_as_default_for_type(t).is_ok() {
            done += 1;
        }
    }
    if done == 0 && !types.is_empty() {
        return Err("The default apps couldn't be changed (~/.config/mimeapps.list).".into());
    }
    Ok(done)
}

/// Whether Slate is what opens text files when they're double-clicked.
fn slate_is_default() -> bool {
    let id = format!("{APP_ID}.desktop");
    gio::AppInfo::default_for_type("text/plain", false).and_then(|d| d.id()).is_some_and(|d| d == id)
}

/// Help → Stop opening files with Slate…: the kinds of files Slate is the default for go back to the system's own
/// choice. Only Slate's entries go from `~/.config/mimeapps.list` (what `make_default` wrote): choices made for other
/// apps stay (GIO's `reset_type_associations` would forget those too).
fn stop_default() -> Result<usize, String> {
    let id = format!("{APP_ID}.desktop");
    let Some(info) = gio::AppInfo::all().into_iter().find(|i| i.id().is_some_and(|s| s == id)) else {
        return Err("Slate isn't installed from its package (the .deb), so it isn't opening any files.".into());
    };
    let path = glib::user_config_dir().join("mimeapps.list");
    let kf = glib::KeyFile::new();
    if kf.load_from_file(&path, glib::KeyFileFlags::KEEP_COMMENTS | glib::KeyFileFlags::KEEP_TRANSLATIONS).is_err() {
        // (no list: nothing was set up)
        return Ok(0);
    }
    let mut n = 0;
    for t in info.supported_types() {
        for group in ["Default Applications", "Added Associations"] {
            let Ok(list) = kf.string_list(group, &t) else { continue };
            let kept: Vec<String> = list.iter().map(|s| s.to_string()).filter(|s| *s != id).collect();
            if kept.len() == list.len() {
                continue;
            }
            if group == "Default Applications" {
                n += 1;
            }
            if kept.is_empty() {
                let _ = kf.remove_key(group, &t);
            } else {
                // (a list as mimeapps.list has them: `a.desktop;b.desktop;`)
                kf.set_string(group, &t, &format!("{};", kept.join(";")));
            }
        }
    }
    if n > 0 {
        kf.save_to_file(&path).map_err(|e| format!("The default apps couldn't be changed ({}): {e}", path.display()))?;
    }
    Ok(n)
}

/// Whether the desktop asks for dark: GNOME's color scheme, GTK's own setting, or a dark GTK theme (Raspberry Pi
/// OS's PiXnoir).
fn system_dark() -> bool {
    if let Some(src) = gio::SettingsSchemaSource::default() {
        if let Some(schema) = src.lookup("org.gnome.desktop.interface", true) {
            if schema.has_key("color-scheme") {
                let s = gio::Settings::new("org.gnome.desktop.interface");
                match s.string("color-scheme").as_str() {
                    "prefer-dark" => return true,
                    "prefer-light" => return false,
                    _ => {}
                }
            }
        }
    }
    let Some(gs) = gtk4::Settings::default() else { return false };
    thread_local! {
        /// GTK's prefer-dark setting as the desktop gave it: Slate sets it itself afterwards (for its own menus and
        /// boxes), so reading it again would only give Slate's own choice back.
        static DESKTOP_PREFERS_DARK: Cell<Option<bool>> = const { Cell::new(None) };
    }
    let prefers = DESKTOP_PREFERS_DARK.with(|c| {
        let v = c.get().unwrap_or_else(|| gs.is_gtk_application_prefer_dark_theme());
        c.set(Some(v));
        v
    });
    if prefers {
        return true;
    }
    gs.gtk_theme_name().is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        n.contains("dark") || n.contains("noir")
    })
}

fn make_style(pango: &pango::Context, s: &Settings) -> view::Style {
    view::Style::new(pango, &s.font, (s.font_size * s.zoom) as f64, s.tab_size, s.wrap, s.line_numbers)
}

fn hex(argb: u32) -> String {
    format!("#{:06x}", argb & 0xFF_FFFF)
}

fn icon_button(icon: &str, tip: &str) -> gtk4::Button {
    let b = gtk4::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b
}

fn text_toggle(label: &str, tip: &str) -> gtk4::ToggleButton {
    let b = gtk4::ToggleButton::with_label(label);
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b
}

/// Builds the window (once: a second start's files come to it through `open`).
pub fn window(application: &gtk4::Application, test: Option<Rc<testmode::Test>>) -> Rc<Ui> {
    if let Some(ui) = get_ui() {
        return ui;
    }
    let settings = Settings::load();
    let (w, h, maximized) = match settings.window {
        Some(p) => (p.w.max(400), p.h.max(300), p.maximized),
        None => (1000, 700, false),
    };
    let window = gtk4::ApplicationWindow::builder().application(application).title("Slate").default_width(w).default_height(h).build();
    if maximized {
        window.maximize();
    }
    window.set_icon_name(Some(APP_ID));

    let text = gtk4::DrawingArea::new();
    text.set_hexpand(true);
    text.set_vexpand(true);
    text.set_focusable(true);
    text.set_cursor_from_name(Some("text"));
    let tabs = gtk4::DrawingArea::new();
    tabs.set_content_height(chrome::TAB_H as i32);
    let status = gtk4::DrawingArea::new();
    status.set_content_height(chrome::STATUS_H as i32);
    let vbar = gtk4::Scrollbar::new(gtk4::Orientation::Vertical, Some(&gtk4::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0)));
    let hbar = gtk4::Scrollbar::new(gtk4::Orientation::Horizontal, Some(&gtk4::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0)));

    // the find bar
    let find_entry = gtk4::Entry::builder().placeholder_text("Find").hexpand(true).build();
    let case_btn = text_toggle("Aa", "Match case");
    let word_btn = text_toggle("ab", "Whole word");
    let regex_btn = text_toggle(".*", "Regular expression");
    let find_count = gtk4::Label::new(None);
    find_count.add_css_class("dim-label");
    let prev = icon_button("go-up-symbolic", "Previous match (Shift+F3)");
    let next = icon_button("go-down-symbolic", "Next match (F3)");
    let close_find = icon_button("window-close-symbolic", "Close (Esc)");
    let find_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    for w in [find_entry.upcast_ref::<gtk4::Widget>(), case_btn.upcast_ref(), word_btn.upcast_ref(), regex_btn.upcast_ref(), find_count.upcast_ref(), prev.upcast_ref(), next.upcast_ref(), close_find.upcast_ref()] {
        find_row.append(w);
    }
    let replace_entry = gtk4::Entry::builder().placeholder_text("Replace with").hexpand(true).build();
    let replace_one = gtk4::Button::with_label("Replace");
    let replace_all = gtk4::Button::with_label("Replace all");
    let replace_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    replace_row.append(&replace_entry);
    replace_row.append(&replace_one);
    replace_row.append(&replace_all);
    let find_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    find_box.add_css_class("slate-bar");
    find_box.append(&find_row);
    find_box.append(&replace_row);
    find_box.set_visible(false);

    // a notice above the text
    let notice_label = gtk4::Label::new(None);
    notice_label.set_wrap(true);
    notice_label.set_xalign(0.0);
    notice_label.set_hexpand(true);
    let notice_close = icon_button("window-close-symbolic", "Dismiss");
    let notice_save_as = gtk4::Button::with_label("Save as…");
    notice_save_as.set_visible(false);
    let notice = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    notice.add_css_class("slate-notice");
    notice.append(&notice_label);
    notice.append(&notice_save_as);
    notice.append(&notice_close);
    notice.set_visible(false);

    let recent_menu = gio::Menu::new();
    let menubar = gtk4::PopoverMenuBar::from_model(Some(&menu_model(&recent_menu)));
    // A menu bar item clicked kept the keyboard once its menu closed (typing went nowhere, as Ctrl+N's new tab
    // showed): as on Windows, the keyboard stays in the text, and the menus open in popovers of their own.
    let mut item = menubar.first_child();
    while let Some(w) = item {
        w.set_focusable(false);
        item = w.next_sibling();
    }
    let middle = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    middle.append(&text);
    middle.append(&vbar);
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&menubar);
    root.append(&tabs);
    root.append(&find_box);
    root.append(&notice);
    root.append(&middle);
    root.append(&hbar);
    root.append(&status);
    window.set_child(Some(&root));

    let status_menu = gtk4::PopoverMenu::from_model(None::<&gio::MenuModel>);
    status_menu.set_parent(&status);
    status_menu.set_has_arrow(false);

    let im = gtk4::IMMulticontext::new();
    let css = gtk4::CssProvider::new();
    css.connect_parsing_error(|_, section, err| eprintln!("css: {} at {}", err, section.to_str()));
    #[allow(deprecated)]
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }

    let style = make_style(&text.pango_context(), &settings);
    let mut app = App::new(settings, style, notifier(), system_dark());
    session::restore(&mut app, test.is_none());
    if app.tabs.is_empty() {
        app.new_untitled();
    }

    let ui = Rc::new(Ui {
        app: RefCell::new(app),
        gtk_app: application.clone(),
        window,
        tabs,
        text,
        status,
        vbar,
        hbar,
        find_box,
        find_entry,
        replace_row,
        replace_entry,
        find_count,
        case_btn,
        word_btn,
        regex_btn,
        notice,
        notice_label,
        notice_save_as,
        status_menu,
        im,
        css,
        strip: RefCell::new(chrome::TabStrip::default()),
        first_tab: Cell::new(0),
        hover: Cell::new(None),
        status_items: RefCell::new(Vec::new()),
        syncing: Cell::new(false),
        scroll_acc: Cell::new(0.0),
        drag_origin: Cell::new((0.0, 0.0)),
        last_input: Cell::new(Instant::now()),
        polling: Cell::new(false),
        css_dark: Cell::new(None),
        closing: Cell::new(false),
        painted: Cell::new(false),
        inhibited: Cell::new(None),
        recent_menu,
        recent_shown: RefCell::new(None),
        is_default: Cell::new(test.is_none() && slate_is_default()),
        test,
    });
    UI.with(|u| *u.borrow_mut() = Some(ui.clone()));
    connect(&ui, &prev, &next, &close_find, &replace_one, &replace_all, &notice_close);
    actions(&ui);
    // timers: the caret, files changed by other programs, the session
    glib::timeout_add_local(Duration::from_millis(BLINK_MS), || {
        if let Some(ui) = get_ui() {
            if STOPPED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                ui.ending();
                ui.closing.set(true);
                ui.gtk_app.quit();
                return glib::ControlFlow::Break;
            }
            ui.blink();
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_local(Duration::from_secs(2), || {
        if let Some(ui) = get_ui() {
            ui.with(|a| a.check_disk());
        }
        glib::ControlFlow::Continue
    });
    // (written on another thread: a big copy on an SD card takes a while)
    glib::timeout_add_local(Duration::from_secs(5), || {
        if let Some(ui) = get_ui() {
            if ui.test.is_none() {
                ui.guarded(|a| {
                    if a.session_dirty {
                        session::start(a, notifier());
                    }
                });
            }
        }
        glib::ControlFlow::Continue
    });
    // Stopped by the system (shutting down, a terminal closed): the session is written first (the caret's timer
    // sees `STOPPED`).
    if ui.test.is_none() {
        extern "C" fn on_signal(_: libc::c_int) {
            STOPPED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(signal, &sa, std::ptr::null_mut());
            }
        }
    }
    ui.apply_theme();
    ui.text.grab_focus();
    ui.refresh();
    ui
}

fn connect(ui: &Rc<Ui>, prev: &gtk4::Button, next: &gtk4::Button, close_find: &gtk4::Button, replace_one: &gtk4::Button, replace_all: &gtk4::Button, notice_close: &gtk4::Button) {
    // drawing
    ui.text.set_draw_func(|area, cr, w, h| {
        if let Some(ui) = get_ui() {
            ui.paint_text(area, cr, w as f64, h as f64);
        }
    });
    // (a panic while drawing is only logged, by the panic hook: showing a message would draw, and fail, again)
    ui.tabs.set_draw_func(|area, cr, w, h| {
        if let Some(ui) = get_ui() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Ok(app) = ui.app.try_borrow() else { return };
                let mut first = ui.first_tab.get();
                let strip = chrome::tab_strip(&app, w as f64, &mut first);
                ui.first_tab.set(first);
                chrome::paint_tabs(cr, &area.pango_context(), &app, &strip, h as f64, ui.hover.get());
                *ui.strip.borrow_mut() = strip;
            }));
        }
    });
    ui.status.set_draw_func(|area, cr, w, h| {
        if let Some(ui) = get_ui() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Ok(app) = ui.app.try_borrow() else { return };
                let items = chrome::paint_status(cr, &area.pango_context(), &app, w as f64, h as f64);
                *ui.status_items.borrow_mut() = items;
            }));
        }
    });

    // keys and typing
    let key = gtk4::EventControllerKey::new();
    key.set_im_context(Some(&ui.im));
    key.connect_key_pressed(|_, keyval, _, state| {
        let Some(ui) = get_ui() else { return glib::Propagation::Proceed };
        if ui.key(keyval, state) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    });
    ui.text.add_controller(key);
    ui.im.set_client_widget(Some(&ui.text));
    ui.im.connect_commit(|_, s| {
        if let Some(ui) = get_ui() {
            ui.touch();
            let s = s.to_string();
            ui.with(|a| a.insert_text(&s));
        }
    });
    let focus = gtk4::EventControllerFocus::new();
    focus.connect_enter(|_| {
        if let Some(ui) = get_ui() {
            ui.im.focus_in();
            ui.with(|a| a.focused = true);
        }
    });
    focus.connect_leave(|_| {
        if let Some(ui) = get_ui() {
            ui.im.focus_out();
            ui.with(|a| a.focused = false);
        }
    });
    ui.text.add_controller(focus);

    // the mouse in the text
    let click = gtk4::GestureClick::new();
    click.set_button(0);
    click.connect_pressed(|g, n, x, y| {
        if let Some(ui) = get_ui() {
            let shift = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
            ui.text_pressed(g.current_button(), n, x, y, shift);
        }
    });
    ui.text.add_controller(click);
    let drag = gtk4::GestureDrag::new();
    drag.connect_drag_begin(|_, x, y| {
        if let Some(ui) = get_ui() {
            ui.drag_origin.set((x, y));
        }
    });
    drag.connect_drag_update(|_, dx, dy| {
        if let Some(ui) = get_ui() {
            let (x, y) = ui.drag_origin.get();
            ui.text_dragged(x + dx, y + dy);
        }
    });
    drag.connect_drag_end(|_, _, _| {
        if let Some(ui) = get_ui() {
            ui.with(|a| a.tab_mut().view.drag = None);
            ui.set_primary();
        }
    });
    ui.text.add_controller(drag);
    let scroll = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::BOTH_AXES);
    scroll.connect_scroll(|c, dx, dy| {
        if let Some(ui) = get_ui() {
            let ctrl = c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            ui.scrolled(c.unit(), dx, dy, ctrl);
        }
        glib::Propagation::Stop
    });
    ui.text.add_controller(scroll);

    // scrollbars
    ui.vbar.adjustment().connect_value_changed(|adj| {
        if let Some(ui) = get_ui() {
            if !ui.syncing.get() {
                let v = adj.value() as u64;
                let g = ui.geom();
                ui.with(|a| a.with_view(&g, |view, cx| view.scroll_to_offset(cx, v)));
            }
        }
    });
    ui.hbar.adjustment().connect_value_changed(|adj| {
        if let Some(ui) = get_ui() {
            if !ui.syncing.get() {
                let v = adj.value();
                ui.with(|a| a.tab_mut().view.scroll_x = v);
            }
        }
    });

    // the tab strip
    let tclick = gtk4::GestureClick::new();
    tclick.set_button(0);
    tclick.connect_pressed(|g, n, x, _| {
        if let Some(ui) = get_ui() {
            ui.tabs_pressed(g.current_button(), n, x);
        }
    });
    ui.tabs.add_controller(tclick);
    let tmotion = gtk4::EventControllerMotion::new();
    tmotion.connect_motion(|_, x, _| {
        if let Some(ui) = get_ui() {
            let part = chrome::tab_part_at(&ui.strip.borrow(), x);
            if part != ui.hover.get() {
                ui.hover.set(part);
                ui.tabs.queue_draw();
            }
        }
    });
    tmotion.connect_leave(|_| {
        if let Some(ui) = get_ui() {
            ui.hover.set(None);
            ui.tabs.queue_draw();
        }
    });
    ui.tabs.add_controller(tmotion);

    // the status bar's items: language, line breaks
    let sclick = gtk4::GestureClick::new();
    sclick.connect_pressed(|_, _, x, _| {
        if let Some(ui) = get_ui() {
            ui.status_pressed(x);
        }
    });
    ui.status.add_controller(sclick);

    // the find bar
    ui.find_entry.connect_changed(|e| {
        if let Some(ui) = get_ui() {
            if !ui.syncing.get() {
                let t = e.text().to_string();
                ui.with(|a| {
                    a.find.query.text = t;
                    a.query_changed();
                    a.find_as_you_type();
                });
            }
        }
    });
    ui.find_entry.connect_activate(|_| {
        if let Some(ui) = get_ui() {
            ui.with(|a| a.exec(Cmd::FindNext));
        }
    });
    for (b, which) in [(&ui.case_btn, 0), (&ui.word_btn, 1), (&ui.regex_btn, 2)] {
        b.connect_toggled(move |b| {
            if let Some(ui) = get_ui() {
                if !ui.syncing.get() {
                    let on = b.is_active();
                    ui.with(|a| {
                        match which {
                            0 => a.find.query.match_case = on,
                            1 => a.find.query.whole_word = on,
                            _ => a.find.query.regex = on,
                        }
                        a.query_changed();
                    });
                }
            }
        });
    }
    let entry_keys = |entry: &gtk4::Entry| {
        let k = gtk4::EventControllerKey::new();
        k.connect_key_pressed(|_, keyval, _, state| {
            let Some(ui) = get_ui() else { return glib::Propagation::Proceed };
            match keyval {
                gdk::Key::Escape => {
                    ui.with(|a| a.exec(Cmd::CloseFind));
                    ui.text.grab_focus();
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter if state.contains(gdk::ModifierType::SHIFT_MASK) => {
                    ui.with(|a| a.exec(Cmd::FindPrev));
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        entry.add_controller(k);
    };
    entry_keys(&ui.find_entry);
    entry_keys(&ui.replace_entry);
    ui.replace_entry.connect_changed(|e| {
        if let Some(ui) = get_ui() {
            if !ui.syncing.get() {
                let t = e.text().to_string();
                ui.with(|a| a.find.replacement = t);
            }
        }
    });
    ui.replace_entry.connect_activate(|_| {
        if let Some(ui) = get_ui() {
            ui.with(|a| a.exec(Cmd::ReplaceOne));
        }
    });
    let cmd_button = |b: &gtk4::Button, cmd: Cmd| {
        b.connect_clicked(move |_| {
            if let Some(ui) = get_ui() {
                ui.with(|a| a.exec(cmd));
                if cmd == Cmd::CloseFind {
                    ui.text.grab_focus();
                }
            }
        });
    };
    cmd_button(prev, Cmd::FindPrev);
    cmd_button(next, Cmd::FindNext);
    cmd_button(close_find, Cmd::CloseFind);
    cmd_button(replace_one, Cmd::ReplaceOne);
    cmd_button(replace_all, Cmd::ReplaceAll);
    ui.notice_save_as.connect_clicked(|_| {
        if let Some(ui) = get_ui() {
            ui.command(Cmd::SaveAs);
        }
    });
    notice_close.connect_clicked(|_| {
        if let Some(ui) = get_ui() {
            ui.with(|a| {
                let tab = a.tab_mut();
                tab.notice = None;
                tab.notice_save_as = false;
            });
        }
    });

    // dropping files on the window opens them
    let drop = gtk4::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(|_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if let Some(ui) = get_ui() {
            ui.with(|a| a.open_paths(&paths));
        }
        true
    });
    ui.window.add_controller(drop);

    ui.window.connect_close_request(|_| {
        let Some(ui) = get_ui() else { return glib::Propagation::Proceed };
        if ui.closing.get() {
            return glib::Propagation::Proceed;
        }
        ui.try_quit();
        if ui.closing.get() { glib::Propagation::Proceed } else { glib::Propagation::Stop }
    });
    ui.window.connect_is_active_notify(|_| {
        if let Some(ui) = get_ui() {
            ui.text.queue_draw();
        }
    });
    if let Some(gs) = gtk4::Settings::default() {
        gs.connect_gtk_theme_name_notify(|_| {
            if let Some(ui) = get_ui() {
                let dark = system_dark();
                ui.with(|a| {
                    a.system_dark = dark;
                    a.apply_theme();
                });
                ui.apply_theme();
            }
        });
    }
}

/// The actions behind the menus and shortcuts.
fn commands() -> Vec<(&'static str, Cmd, &'static [&'static str])> {
    vec![
        ("new-tab", Cmd::NewTab, &["<Control>n", "<Control>t"]),
        ("open", Cmd::Open, &["<Control>o"]),
        ("save", Cmd::Save, &["<Control>s"]),
        ("save-as", Cmd::SaveAs, &["<Control><Shift>s"]),
        ("save-all", Cmd::SaveAll, &["<Control><Alt>s"]),
        ("close-tab", Cmd::CloseTab, &["<Control>w"]),
        ("reopen-closed", Cmd::ReopenClosed, &["<Control><Shift>t"]),
        ("quit", Cmd::Quit, &["<Control>q"]),
        ("undo", Cmd::Undo, &["<Control>z"]),
        ("redo", Cmd::Redo, &["<Control>y", "<Control><Shift>z"]),
        ("cut", Cmd::Cut, &["<Control>x"]),
        ("copy", Cmd::Copy, &["<Control>c"]),
        ("paste", Cmd::Paste, &["<Control>v"]),
        ("select-all", Cmd::SelectAll, &["<Control>a"]),
        ("find", Cmd::Find, &["<Control>f"]),
        ("replace", Cmd::Replace, &["<Control>h"]),
        ("find-next", Cmd::FindNext, &["F3"]),
        ("find-prev", Cmd::FindPrev, &["<Shift>F3"]),
        ("replace-one", Cmd::ReplaceOne, &[]),
        ("replace-all", Cmd::ReplaceAll, &["<Control><Alt>Return"]),
        ("goto-line", Cmd::GotoLine, &["<Control>g"]),
        ("toggle-comment", Cmd::ToggleComment, &["<Control>slash"]),
        ("duplicate-line", Cmd::DuplicateLine, &["<Control>d"]),
        ("delete-line", Cmd::DeleteLine, &["<Control><Shift>k"]),
        ("sort-asc", Cmd::Lines(LineOp::SortAsc), &[]),
        ("sort-desc", Cmd::Lines(LineOp::SortDesc), &[]),
        ("dedupe", Cmd::Lines(LineOp::Dedupe), &[]),
        ("remove-blank", Cmd::Lines(LineOp::RemoveBlank), &[]),
        ("trim", Cmd::Lines(LineOp::TrimTrailing), &[]),
        ("upper", Cmd::Case(CaseOp::Upper), &["<Control><Shift>u"]),
        ("lower", Cmd::Case(CaseOp::Lower), &["<Control>u"]),
        ("title-case", Cmd::Case(CaseOp::Title), &[]),
        ("format", Cmd::Format, &["<Shift><Alt>f"]),
        ("json-format", Cmd::JsonFormat, &[]),
        ("json-minify", Cmd::JsonMinify, &[]),
        ("json-check", Cmd::JsonCheck, &[]),
        ("xml-format", Cmd::XmlFormat, &[]),
        ("xml-minify", Cmd::XmlMinify, &[]),
        ("xml-check", Cmd::XmlCheck, &[]),
        ("wrap", Cmd::Wrap, &["<Alt>z"]),
        ("line-numbers", Cmd::LineNumbers, &[]),
        ("zoom-in", Cmd::ZoomIn, &["<Control>plus", "<Control>equal", "<Control>KP_Add"]),
        ("zoom-out", Cmd::ZoomOut, &["<Control>minus", "<Control>KP_Subtract"]),
        ("zoom-reset", Cmd::ZoomReset, &["<Control>0"]),
        ("theme-system", Cmd::Theme(ThemeMode::System), &[]),
        ("theme-light", Cmd::Theme(ThemeMode::Light), &[]),
        ("theme-dark", Cmd::Theme(ThemeMode::Dark), &[]),
        ("next-tab", Cmd::NextTab, &["<Control>Tab", "<Control>Page_Down"]),
        ("prev-tab", Cmd::PrevTab, &["<Control><Shift>Tab", "<Control>Page_Up"]),
        ("eol-lf", Cmd::SetEol(Eol::Lf), &[]),
        ("eol-crlf", Cmd::SetEol(Eol::Crlf), &[]),
        ("date-time", Cmd::InsertDateTime, &["F5"]),
        ("shortcuts", Cmd::Shortcuts, &["<Control>question"]),
        ("about", Cmd::About, &[]),
        ("make-default", Cmd::MakeDefault, &[]),
        ("stop-default", Cmd::StopDefault, &[]),
    ]
}

/// Menu items that show a check mark: their actions have a true/false state (`Ui::sync_menu`).
const CHECKED: &[&str] = &["wrap", "line-numbers", "theme-system", "theme-light", "theme-dark", "eol-lf", "eol-crlf"];

fn actions(ui: &Rc<Ui>) {
    for (name, cmd, accels) in commands() {
        let a = if CHECKED.contains(&name) {
            gio::SimpleAction::new_stateful(name, None, &false.to_variant())
        } else {
            gio::SimpleAction::new(name, None)
        };
        a.connect_activate(move |_, _| {
            if let Some(ui) = get_ui() {
                ui.command(cmd);
            }
        });
        ui.window.add_action(&a);
        if !accels.is_empty() {
            ui.gtk_app.set_accels_for_action(&format!("win.{name}"), accels);
        }
    }
    for k in 1..=9usize {
        let name = format!("tab-{k}");
        let a = gio::SimpleAction::new(&name, None);
        a.connect_activate(move |_, _| {
            if let Some(ui) = get_ui() {
                ui.command(Cmd::GoTab(k));
            }
        });
        ui.window.add_action(&a);
        // (Ctrl as on Windows; Alt as in other Linux apps)
        ui.gtk_app.set_accels_for_action(&format!("win.{name}"), &[&format!("<Control>{k}"), &format!("<Alt>{k}")]);
    }
    // (its state is the tab's language: the menu shows a dot at that one)
    let lang = gio::SimpleAction::new_stateful("set-lang", Some(glib::VariantTy::STRING), &"".to_variant());
    lang.connect_activate(|_, v| {
        let Some(label) = v.and_then(|v| v.get::<String>()) else { return };
        if let (Some(ui), Some(&l)) = (get_ui(), Lang::ALL.iter().find(|l| l.label() == label)) {
            ui.command(Cmd::SetLang(l));
        }
    });
    ui.window.add_action(&lang);
    let recent = gio::SimpleAction::new("open-recent", Some(glib::VariantTy::STRING));
    recent.connect_activate(|_, v| {
        let Some(p) = v.and_then(|v| v.get::<String>()) else { return };
        if let Some(ui) = get_ui() {
            ui.with(|a| a.open_paths(&[PathBuf::from(p)]));
        }
    });
    ui.window.add_action(&recent);
}

/// Menu items shown only while their action is on: JSON's and XML's for those files (as on Windows), and Help's
/// Open files with Slate… or Stop opening files with Slate…, whichever applies (`Ui::sync_menu`).
const SHOWN_WHEN_ON: &[&str] =
    &["json-format", "json-minify", "json-check", "xml-format", "xml-minify", "xml-check", "make-default", "stop-default"];

fn section(items: &[(&str, &str)]) -> gio::Menu {
    let commands = commands();
    let m = gio::Menu::new();
    for (label, action) in items {
        let item = gio::MenuItem::new(Some(label), Some(&format!("win.{action}")));
        // GTK shows no shortcut for an action that has more than one (New tab, Redo, Zoom in): the first is the one
        // shown, as on Windows. Format JSON and Format XML show Format's.
        if let Some((_, _, accels)) = commands.iter().find(|(n, _, a)| n == action && a.len() > 1) {
            item.set_attribute_value("accel", Some(&accels[0].to_variant()));
        }
        if matches!(*action, "json-format" | "xml-format") {
            item.set_attribute_value("accel", Some(&"<Shift><Alt>f".to_variant()));
        }
        if SHOWN_WHEN_ON.contains(action) {
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        }
        m.append_item(&item);
    }
    m
}

fn menu_model(recent: &gio::Menu) -> gio::Menu {
    let bar = gio::Menu::new();
    let file = gio::Menu::new();
    let open = section(&[("New tab", "new-tab"), ("Open…", "open")]);
    open.append_submenu(Some("Open recent"), recent);
    file.append_section(None, &open);
    file.append_section(None, &section(&[("Save", "save"), ("Save as…", "save-as"), ("Save all", "save-all")]));
    file.append_section(None, &section(&[("Close tab", "close-tab"), ("Reopen closed tab", "reopen-closed"), ("Quit", "quit")]));
    bar.append_submenu(Some("_File"), &file);
    let edit = gio::Menu::new();
    edit.append_section(None, &section(&[("Undo", "undo"), ("Redo", "redo")]));
    edit.append_section(None, &section(&[("Cut", "cut"), ("Copy", "copy"), ("Paste", "paste"), ("Select all", "select-all")]));
    edit.append_section(None, &section(&[("Find…", "find"), ("Replace…", "replace"), ("Find next", "find-next"), ("Find previous", "find-prev"), ("Go to line…", "goto-line")]));
    edit.append_section(None, &section(&[("Toggle comment", "toggle-comment"), ("Duplicate line", "duplicate-line"), ("Delete line", "delete-line")]));
    let lines = section(&[("Sort A to Z", "sort-asc"), ("Sort Z to A", "sort-desc"), ("Remove duplicate lines", "dedupe"), ("Remove blank lines", "remove-blank"), ("Trim spaces at line ends", "trim")]);
    edit.append_submenu(Some("Lines"), &lines);
    let case = section(&[("UPPERCASE", "upper"), ("lowercase", "lower"), ("Title Case", "title-case")]);
    edit.append_submenu(Some("Change case"), &case);
    edit.append_section(None, &section(&[("Time/date", "date-time")]));
    bar.append_submenu(Some("_Edit"), &edit);
    let view = gio::Menu::new();
    view.append_section(None, &section(&[("Word wrap", "wrap"), ("Line numbers", "line-numbers")]));
    view.append_section(None, &section(&[("Zoom in", "zoom-in"), ("Zoom out", "zoom-out"), ("Reset zoom", "zoom-reset")]));
    let theme = section(&[("Use system setting", "theme-system"), ("Light", "theme-light"), ("Dark", "theme-dark")]);
    view.append_submenu(Some("Theme"), &theme);
    bar.append_submenu(Some("_View"), &view);
    let format = gio::Menu::new();
    format.append_section(None, &section(&[("Format JSON", "json-format"), ("Minify JSON", "json-minify"), ("Check JSON", "json-check")]));
    format.append_section(None, &section(&[("Format XML", "xml-format"), ("Minify XML", "xml-minify"), ("Check XML", "xml-check")]));
    let rest = gio::Menu::new();
    rest.append_submenu(Some("Language"), &lang_menu());
    rest.append_submenu(Some("Line endings"), &eol_menu());
    format.append_section(None, &rest);
    bar.append_submenu(Some("F_ormat"), &format);
    let help = gio::Menu::new();
    help.append_section(None, &section(&[("Keyboard shortcuts", "shortcuts"), ("Open files with Slate…", "make-default"), ("Stop opening files with Slate…", "stop-default"), ("About Slate", "about")]));
    bar.append_submenu(Some("_Help"), &help);
    bar
}

fn eol_menu() -> gio::Menu {
    section(&[("Windows (CRLF)", "eol-crlf"), ("Unix (LF)", "eol-lf")])
}

/// File → Open recent: the files opened last (`Ui::sync_menu` keeps it current).
fn fill_recent(menu: &gio::Menu, recent: &[PathBuf]) {
    menu.remove_all();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for p in recent {
        // (a name that isn't UTF-8 can't be a menu's target)
        let Some(target) = p.to_str() else { continue };
        let shown = match home.as_ref().and_then(|h| p.strip_prefix(h).ok()) {
            Some(rest) => format!("~/{}", rest.display()),
            None => p.display().to_string(),
        };
        // (an underscore would be taken for an access key)
        let item = gio::MenuItem::new(Some(&shown.replace('_', "__")), None);
        item.set_action_and_target_value(Some("win.open-recent"), Some(&target.to_variant()));
        menu.append_item(&item);
    }
    if menu.n_items() == 0 {
        menu.append(Some("No files opened yet"), None);
    }
}

fn lang_menu() -> gio::Menu {
    let m = gio::Menu::new();
    let mut langs: Vec<Lang> = Lang::ALL.to_vec();
    langs.sort_by_key(|l| (*l != Lang::Plain, l.label().to_ascii_lowercase()));
    for l in langs {
        let item = gio::MenuItem::new(Some(l.label()), None);
        item.set_action_and_target_value(Some("win.set-lang"), Some(&l.label().to_variant()));
        m.append_item(&item);
    }
    m
}

const SHORTCUTS: &str = "Slate keyboard shortcuts

  Ctrl+N / Ctrl+T          New tab
  Ctrl+O                   Open
  Ctrl+S / Ctrl+Shift+S    Save / Save as
  Ctrl+Alt+S               Save all
  Ctrl+W / Ctrl+Shift+T    Close tab / reopen the tab closed last
  Ctrl+Tab, Ctrl+PgDn      Next tab (Ctrl+Shift+Tab, Ctrl+PgUp: previous)
  Ctrl+1 … Ctrl+9          Go to a tab (9: the last; Alt+1 … Alt+9 too)
  Ctrl+Q                   Quit

  Ctrl+Z / Ctrl+Y          Undo / redo (Ctrl+Shift+Z: redo too)
  Ctrl+X, C, V, A          Cut, copy, paste, select all (with nothing selected: the line)
  Ctrl+F / Ctrl+H          Find / replace
  F3 / Shift+F3            Next / previous match (in the find box: Enter / Shift+Enter)
  Ctrl+Alt+Enter           Replace all
  Esc                      Close the find bar
  Ctrl+G                   Go to line
  Ctrl+/                   Toggle comment
  Ctrl+D / Ctrl+Shift+K    Duplicate / delete the line
  Alt+Up / Alt+Down        Move the line up / down
  Tab / Shift+Tab          Indent / outdent (with lines selected)
  Ctrl+U / Ctrl+Shift+U    lowercase / UPPERCASE
  Shift+Alt+F              Format JSON or XML
  F5                       Insert the time and date

  Alt+Z                    Word wrap
  Ctrl+Plus / Ctrl+Minus   Zoom in / out (Ctrl+0: reset)
  Ctrl+?                   This list
";

impl Ui {
    /// Test mode: a key as if pressed in the text.
    pub fn key_for_test(&self, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        self.key(keyval, state)
    }

    pub fn find_entry_for_test(&self) -> &gtk4::Entry {
        &self.find_entry
    }

    pub fn replace_entry_for_test(&self) -> &gtk4::Entry {
        &self.replace_entry
    }

    /// The window may close without asking (the end of a test).
    pub fn closing_now(&self) {
        self.closing.set(true);
    }

    /// Runs `f` on the app, then shows what came of it.
    pub fn with<R>(&self, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = self.guarded(f);
        self.refresh();
        r
    }

    /// Runs `f` on the app (None if the app is busy, or `f` panicked). A panic in a GTK callback would end Slate
    /// there and then, so it's caught: logged in crash.log (by the panic hook), the session written, and the window
    /// says so.
    fn guarded<R>(&self, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = {
            let mut a = self.app.try_borrow_mut().ok()?;
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut a)))
        };
        match r {
            Ok(r) => Some(r),
            Err(_) => {
                if let Ok(mut a) = self.app.try_borrow_mut() {
                    if self.test.is_none() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session::save(&mut a)));
                    }
                    a.asks.clear();
                    a.flash("Something went wrong (details in crash.log in Slate's data folder). Your work is kept.", true);
                }
                None
            }
        }
    }

    /// The desktop session is ending (logging out, shutting down) or Slate was told to stop: the session is written
    /// now, so everything up to the last key comes back next time. Unsaved work it can't keep makes logging out wait
    /// (where the session manager does), and the window shows it.
    pub fn ending(&self) {
        let unkept = self
            .guarded(|a| {
                if self.test.is_none() {
                    session::save(a);
                    self.save_settings(a);
                }
                (0..a.tabs.len()).any(|i| a.tabs[i].doc.is_dirty() && !session::keeps(a, i))
            })
            .unwrap_or(false);
        if unkept && self.inhibited.get().is_none() {
            let cookie = self.gtk_app.inhibit(Some(&self.window), gtk4::ApplicationInhibitFlags::LOGOUT, Some("Unsaved changes"));
            if cookie != 0 {
                self.inhibited.set(Some(cookie));
            }
            self.window.present();
        }
    }

    fn save_settings(&self, a: &mut App) {
        let (w, h) = self.window.default_size();
        a.settings.window = Some(crate::settings::Placement { x: 0, y: 0, w, h, maximized: self.window.is_maximized() });
        if crate::settings::persist() {
            a.settings.save();
        }
    }

    fn touch(&self) {
        self.last_input.set(Instant::now());
        if let Ok(mut a) = self.app.try_borrow_mut() {
            a.caret_on = true;
        }
    }

    pub fn command(&self, cmd: Cmd) {
        self.touch();
        // In a box of the find bar, the clipboard and undo keys are its own.
        if let Some(text) = GtkWindowExt::focus(&self.window).and_then(|w| w.downcast::<gtk4::Text>().ok()) {
            let own = match cmd {
                Cmd::Cut => Some("clipboard.cut"),
                Cmd::Copy => Some("clipboard.copy"),
                Cmd::Paste => Some("clipboard.paste"),
                Cmd::SelectAll => Some("selection.select-all"),
                Cmd::Undo => Some("text.undo"),
                Cmd::Redo => Some("text.redo"),
                _ => None,
            };
            if let Some(name) = own {
                let _ = text.activate_action(name, None);
                return;
            }
        }
        self.with(|a| a.exec(cmd));
        match cmd {
            Cmd::Find | Cmd::Replace => {
                self.find_entry.grab_focus();
                self.find_entry.select_region(0, -1);
            }
            // A new tab is for typing; other commands leave the keyboard in the text too (wherever it was, a menu
            // or a button), unless it's in the find bar's boxes.
            Cmd::CloseFind | Cmd::NewTab => {
                self.text.grab_focus();
            }
            _ => {
                if !GtkWindowExt::focus(&self.window).is_some_and(|f| f.is_ancestor(&self.find_box)) {
                    self.text.grab_focus();
                }
            }
        }
    }

    fn gutter_width(&self, app: &App) -> f64 {
        if !app.style.line_numbers {
            return 0.0;
        }
        let doc = &app.tab().doc;
        let lines = doc.line_count().unwrap_or(doc.len() / 40 + 1).max(1);
        let digits = (lines as f64).log10().floor() as usize + 1;
        digits.max(3) as f64 * app.style.char_w + 2.0 * PAD
    }

    /// The text area as the view sees it.
    fn geom(&self) -> Geom {
        let gw = self.app.try_borrow().map(|a| self.gutter_width(&a)).unwrap_or(40.0);
        let w = self.text.width().max(100) as f64;
        Geom { pango: self.text.pango_context(), width: (w - gw - PAD * 2.0).max(40.0), height: self.text.height().max(40) as f64 }
    }

    fn key(&self, keyval: gdk::Key, state: gdk::ModifierType) -> bool {
        let _timing = Timing("key", Instant::now());
        self.touch();
        let g = self.geom();
        let handled = self.guarded(|a| a.on_key(&g, keyval, state)).unwrap_or(false);
        if handled {
            self.refresh();
        }
        handled
    }

    fn text_pressed(&self, button: u32, n: i32, x: f64, y: f64, shift: bool) {
        self.touch();
        self.text.grab_focus();
        let g = self.geom();
        let gw = self.app.try_borrow().map(|a| self.gutter_width(&a)).unwrap_or(0.0);
        let x = x - gw - PAD;
        if button == 2 {
            // Linux: the middle button pastes what was selected last (anywhere), where it's clicked
            let pos = self.with(|a| a.with_view(&g, |v, cx| v.pos_at(cx, x, y))).unwrap_or(0);
            let primary = gdk::Display::default().map(|d| d.primary_clipboard());
            if let (Some(cb), None) = (primary, &self.test) {
                glib::spawn_future_local(async move {
                    if let Ok(Some(t)) = cb.read_text_future().await {
                        if let Some(ui) = get_ui() {
                            let t = t.to_string();
                            ui.with(|a| {
                                a.tab_mut().view.sel = Sel::at(pos);
                                a.paste_text(&t);
                            });
                        }
                    }
                });
            }
            return;
        }
        if button != 1 {
            if button == 3 {
                self.context_menu(x + gw + PAD, y);
            }
            return;
        }
        self.with(|a| {
            let doc_len = a.tab().doc.len();
            a.with_view(&g, |v, cx| {
                let pos = v.pos_at(cx, x, y).min(doc_len);
                match n {
                    1 => {
                        v.set_caret(pos, shift);
                        v.drag = Some(view::Drag::Chars);
                    }
                    2 => {
                        let (wa, wb) = cx.doc.word_at(pos);
                        v.sel = Sel::new(wa, wb);
                        v.drag = Some(view::Drag::Words(wa, wb));
                    }
                    _ => {
                        let a0 = cx.doc.line_start_of(pos);
                        let b0 = cx.doc.next_newline(a0).map_or(cx.doc.len(), |p| p + 1);
                        v.sel = Sel::new(a0, b0);
                        v.drag = Some(view::Drag::Lines(a0, b0));
                    }
                }
                v.want_x = None;
            });
            a.dirty_view = true;
        });
    }

    /// What's selected becomes the primary selection (what a middle click pastes, in any program).
    fn set_primary(&self) {
        if self.test.is_some() {
            return;
        }
        let text = self.app.try_borrow().ok().and_then(|a| {
            let tab = a.tab();
            let sel = tab.view.sel;
            (!sel.is_empty() && sel.end() - sel.start() <= 4 << 20)
                .then(|| String::from_utf8_lossy(&tab.doc.read(sel.start(), sel.end())).into_owned())
        });
        if let (Some(t), Some(d)) = (text, gdk::Display::default()) {
            d.primary_clipboard().set_text(&t);
        }
    }

    fn text_dragged(&self, x: f64, y: f64) {
        let g = self.geom();
        let gw = self.app.try_borrow().map(|a| self.gutter_width(&a)).unwrap_or(0.0);
        let x = x - gw - PAD;
        self.with(|a| {
            a.with_view(&g, |v, cx| {
                if y < 0.0 {
                    v.scroll_rows(cx, -1);
                } else if y > cx.height {
                    v.scroll_rows(cx, 1);
                }
                v.layout_rows(cx);
                let pos = v.pos_at(cx, x, y.clamp(0.0, cx.height - 1.0));
                match v.drag {
                    Some(view::Drag::Chars) => v.sel.caret = pos,
                    Some(view::Drag::Words(wa, wb)) => {
                        let (pa, pb) = cx.doc.word_at(pos);
                        v.sel = if pos < wa { Sel::new(wb, pa) } else { Sel::new(wa, pb.max(wb)) };
                    }
                    Some(view::Drag::Lines(la, lb)) => {
                        let a0 = cx.doc.line_start_of(pos);
                        let b0 = cx.doc.next_newline(a0).map_or(cx.doc.len(), |p| p + 1);
                        v.sel = if a0 < la { Sel::new(lb, a0) } else { Sel::new(la, b0.max(lb)) };
                    }
                    None => {}
                }
            });
            a.dirty_view = true;
        });
    }

    fn scrolled(&self, unit: gdk::ScrollUnit, dx: f64, dy: f64, ctrl: bool) {
        if ctrl {
            if dy < 0.0 {
                self.command(Cmd::ZoomIn);
            } else if dy > 0.0 {
                self.command(Cmd::ZoomOut);
            }
            return;
        }
        let g = self.geom();
        let row_h = self.app.try_borrow().map(|a| a.style.row_h).unwrap_or(20.0);
        let rows = match unit {
            gdk::ScrollUnit::Surface => {
                let acc = self.scroll_acc.get() + dy / row_h;
                let whole = acc.trunc();
                self.scroll_acc.set(acc - whole);
                whole as i64
            }
            _ => (dy * 3.0).round() as i64,
        };
        self.with(|a| {
            let wrap = a.style.wrap;
            a.with_view(&g, |v, cx| {
                if rows != 0 {
                    v.scroll_rows(cx, rows);
                }
                if !wrap && dx != 0.0 {
                    let step = if unit == gdk::ScrollUnit::Surface { dx } else { dx * cx.style.char_w * 6.0 };
                    v.scroll_x = (v.scroll_x + step).clamp(0.0, (v.max_w - cx.width + cx.style.char_w * 4.0).max(0.0));
                }
            });
            a.dirty_view = true;
        });
    }

    fn tabs_pressed(&self, button: u32, n: i32, x: f64) {
        let part = chrome::tab_part_at(&self.strip.borrow(), x);
        match (part, button) {
            (Some(chrome::TabPart::Tab(i)), 1) => {
                self.with(|a| a.activate(i));
            }
            (Some(chrome::TabPart::Tab(i)) | Some(chrome::TabPart::Close(i)), 2) | (Some(chrome::TabPart::Close(i)), 1) => {
                self.with(|a| a.close_tab(i));
            }
            (Some(chrome::TabPart::Plus), 1) => {
                self.with(|a| a.exec(Cmd::NewTab));
            }
            (Some(chrome::TabPart::Left), 1) => {
                self.first_tab.set(self.first_tab.get().saturating_sub(1));
                self.tabs.queue_draw();
            }
            (Some(chrome::TabPart::Right), 1) => {
                self.first_tab.set(self.first_tab.get() + 1);
                self.tabs.queue_draw();
            }
            (None, 1) if n == 2 => {
                self.with(|a| a.exec(Cmd::NewTab));
            }
            _ => {}
        }
        self.text.grab_focus();
    }

    fn status_pressed(&self, x: f64) {
        let items = self.status_items.borrow().clone();
        let Some(&(k, ix, iw)) = items.iter().find(|(_, ix, iw)| x >= *ix && x < ix + iw) else { return };
        let menu = match k {
            1 => lang_menu(),
            2 => eol_menu(),
            _ => return,
        };
        self.status_menu.set_menu_model(Some(&menu));
        self.status_menu.set_pointing_to(Some(&gdk::Rectangle::new(ix as i32, 0, iw as i32, chrome::STATUS_H as i32)));
        self.status_menu.popup();
    }

    fn context_menu(&self, x: f64, y: f64) {
        let menu = gio::Menu::new();
        menu.append_section(None, &section(&[("Cut", "cut"), ("Copy", "copy"), ("Paste", "paste")]));
        menu.append_section(None, &section(&[("Select all", "select-all")]));
        let pop = gtk4::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&self.text);
        pop.set_has_arrow(false);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    fn blink(&self) {
        let Ok(mut a) = self.app.try_borrow_mut() else { return };
        if !a.focused || !self.window.is_active() {
            return;
        }
        if self.last_input.get().elapsed() > BLINK_STOP {
            if !a.caret_on {
                a.caret_on = true;
                drop(a);
                self.text.queue_draw();
            }
            return;
        }
        a.caret_on = !a.caret_on;
        drop(a);
        self.text.queue_draw();
    }

    /// Background work finished (or is still running: then again soon).
    fn poll(&self) {
        // (the app busy: looked at again soon)
        let busy = self.app.try_borrow_mut().is_err();
        let running = busy || self.guarded(|a| a.poll_jobs()).unwrap_or(false);
        self.refresh();
        if running && !self.polling.get() {
            self.polling.set(true);
            glib::timeout_add_local_once(Duration::from_millis(100), || {
                if let Some(ui) = get_ui() {
                    ui.polling.set(false);
                    ui.poll();
                }
            });
        }
    }

    pub fn apply_theme(&self) {
        let Ok(a) = self.app.try_borrow() else { return };
        let t = a.theme.clone();
        let dark = a.dark();
        drop(a);
        if let Some(gs) = gtk4::Settings::default() {
            gs.set_gtk_application_prefer_dark_theme(dark);
        }
        self.css_dark.set(Some(dark));
        let css = format!(
            ".slate-bar {{ background-color: {frame}; padding: 6px 10px; border-bottom: 1px solid {border}; }}
             .slate-notice {{ background-color: {notice}; color: {text}; padding: 6px 12px; }}
             .slate-notice label {{ color: {text}; }}
             menubar {{ background-color: {frame}; color: {text}; }}
             menubar > item {{ color: {text}; }}
             window {{ background-color: {frame}; }}",
            frame = hex(t.frame),
            border = hex(t.border),
            notice = hex(t.notice_bg),
            text = hex(t.text),
        );
        self.css.load_from_data(&css);
        self.queue_all();
    }

    fn queue_all(&self) {
        self.text.queue_draw();
        self.tabs.queue_draw();
        self.status.queue_draw();
    }

    /// Shows what the app asked for and what changed: dialogs, the clipboard, the title, the find bar, a redraw.
    pub fn refresh(&self) {
        // (a panic in here is logged by the panic hook; Slate carries on)
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.refresh_now()));
    }

    fn refresh_now(&self) {
        loop {
            let asks = match self.app.try_borrow_mut() {
                Ok(mut a) => std::mem::take(&mut a.asks),
                Err(_) => return,
            };
            if asks.is_empty() {
                break;
            }
            for ask in asks {
                self.ask(ask);
            }
        }
        let Ok(mut a) = self.app.try_borrow_mut() else { return };
        if let Some(t) = a.copy_out.take() {
            match &self.test {
                Some(test) => *test.clipboard.borrow_mut() = t,
                None => self.text.clipboard().set_text(&t),
            }
        }
        if std::mem::take(&mut a.paste_wanted) {
            match &self.test {
                Some(test) => {
                    let t = test.clipboard.borrow().clone();
                    a.paste_text(&t);
                }
                None => {
                    let cb = self.text.clipboard();
                    glib::spawn_future_local(async move {
                        if let Ok(Some(t)) = cb.read_text_future().await {
                            if let Some(ui) = get_ui() {
                                let t = t.to_string();
                                ui.with(|a| a.paste_text(&t));
                            }
                        }
                    });
                }
            }
        }
        if std::mem::take(&mut a.restyle) {
            a.style = make_style(&self.text.pango_context(), &a.settings);
            for t in &mut a.tabs {
                t.view.clear_cache();
            }
        }
        let theme_changed = a.theme.dark != a.dark();
        if theme_changed {
            a.apply_theme();
        }
        self.sync_menu(&a);
        if std::mem::take(&mut a.reveal_pending) {
            let center = std::mem::take(&mut a.reveal_center);
            drop(a);
            let g = self.geom();
            let Ok(mut b) = self.app.try_borrow_mut() else { return };
            b.with_view(&g, |v, cx| v.reveal(cx, center));
            a = b;
        }
        if std::mem::take(&mut a.dirty_title) {
            let tab = a.tab();
            let mut title = tab.title();
            if tab.doc.is_dirty() {
                title = format!("• {title}");
            }
            if let Some(dir) = tab.doc.path.as_ref().and_then(|p| p.parent()) {
                title = format!("{title} ({})", dir.display());
            }
            self.window.set_title(Some(&format!("{title} — Slate")));
        }
        // the find bar
        self.syncing.set(true);
        self.find_box.set_visible(a.find.open);
        self.replace_row.set_visible(a.find.replace);
        if self.find_entry.text().as_str() != a.find.query.text {
            self.find_entry.set_text(&a.find.query.text);
        }
        self.case_btn.set_active(a.find.query.match_case);
        self.word_btn.set_active(a.find.query.whole_word);
        self.regex_btn.set_active(a.find.query.regex);
        let count = match (&a.find.error, &a.find.found) {
            (Some(e), _) => e.clone(),
            (None, Some((_, f))) if f.count == 0 => "No results".into(),
            (None, Some((_, f))) => {
                let sel = a.tab().view.sel;
                let k = f.positions.iter().position(|p| *p == (sel.start(), sel.end()));
                match k {
                    Some(k) => format!("{} of {}", k + 1, f.count),
                    None => format!("{} found", f.count),
                }
            }
            _ => String::new(),
        };
        self.find_count.set_text(&count);
        // the notice
        match &a.tab().notice {
            Some(n) => {
                self.notice_label.set_text(n);
                self.notice.set_visible(true);
                self.notice_save_as.set_visible(a.tab().notice_save_as);
            }
            None => self.notice.set_visible(false),
        }
        self.hbar.set_visible(!a.style.wrap);
        // (logging out waited for unsaved work the session can't keep: not any more once it's dealt with)
        if let Some(cookie) = self.inhibited.get() {
            if !(0..a.tabs.len()).any(|i| a.tabs[i].doc.is_dirty() && !session::keeps(&a, i)) {
                self.gtk_app.uninhibit(cookie);
                self.inhibited.set(None);
            }
        }
        let find_open = a.find.open;
        self.syncing.set(false);
        let dark = a.theme.dark;
        drop(a);
        if self.css_dark.get() != Some(dark) {
            self.apply_theme();
        }
        if !find_open && GtkWindowExt::focus(&self.window).is_some_and(|f| f.is_ancestor(&self.find_box)) {
            self.text.grab_focus();
        }
        self.queue_all();
    }

    /// The menus' check marks and dots follow the app: Word wrap, Line numbers, the theme, the tab's line breaks and
    /// language.
    fn sync_menu(&self, a: &App) {
        let tab = a.tab();
        let theme = a.settings.theme;
        let checks = [
            ("wrap", a.style.wrap),
            ("line-numbers", a.style.line_numbers),
            ("theme-system", theme == ThemeMode::System),
            ("theme-light", theme == ThemeMode::Light),
            ("theme-dark", theme == ThemeMode::Dark),
            ("eol-lf", tab.doc.eol == Eol::Lf),
            ("eol-crlf", tab.doc.eol == Eol::Crlf),
        ];
        for (name, on) in checks {
            self.set_action_state(name, on.to_variant());
        }
        self.set_action_state("set-lang", tab.lang.label().to_variant());
        // JSON's and XML's items only for those files; Open files or Stop opening files with Slate, whichever applies
        let default = self.is_default.get();
        let on = [
            ("json-format", tab.lang == Lang::Json),
            ("json-minify", tab.lang == Lang::Json),
            ("json-check", tab.lang == Lang::Json),
            ("xml-format", tab.lang == Lang::Xml),
            ("xml-minify", tab.lang == Lang::Xml),
            ("xml-check", tab.lang == Lang::Xml),
            ("make-default", !default),
            ("stop-default", default),
        ];
        for (name, enabled) in on {
            if let Some(action) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                if action.is_enabled() != enabled {
                    action.set_enabled(enabled);
                }
            }
        }
        if self.recent_shown.borrow().as_deref() != Some(a.settings.recent.as_slice()) {
            fill_recent(&self.recent_menu, &a.settings.recent);
            *self.recent_shown.borrow_mut() = Some(a.settings.recent.clone());
        }
    }

    fn set_action_state(&self, name: &str, state: glib::Variant) {
        if let Some(action) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            if action.state().as_ref() != Some(&state) {
                action.set_state(&state);
            }
        }
    }

    /// After painting: the scrollbars show where the view is.
    fn sync_scrollbars(&self) {
        let Ok(a) = self.app.try_borrow() else { return };
        let tab = a.tab();
        let len = tab.doc.len().max(1) as f64;
        let v = &tab.view;
        let shown = v.rows.last().map(|r| r.seg.end + r.seg.eol as u64).unwrap_or(0).saturating_sub(v.top).max(1) as f64;
        let max_w = v.max_w;
        let scroll_x = v.scroll_x;
        let top = v.top as f64;
        let width = self.geom().width;
        drop(a);
        self.syncing.set(true);
        let adj = self.vbar.adjustment();
        adj.configure(top, 0.0, len.max(shown), (shown / 3.0).max(1.0), shown, shown);
        let h = self.hbar.adjustment();
        h.configure(scroll_x, 0.0, (max_w + 40.0).max(width), 20.0, width, width);
        self.syncing.set(false);
    }

    fn paint_text(&self, area: &gtk4::DrawingArea, cr: &cairo::Context, w: f64, h: f64) {
        // (a panic while drawing is only logged: showing a message would draw, and fail, again)
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.paint_text_now(area, cr, w, h)));
        if !self.painted.replace(true) && self.test.is_none() {
            session::restored();
        }
    }

    fn paint_text_now(&self, area: &gtk4::DrawingArea, cr: &cairo::Context, w: f64, h: f64) {
        let started = Instant::now();
        let _timing = Timing("paint", started);
        let Ok(mut a) = self.app.try_borrow_mut() else { return };
        set_color(cr, a.theme.surface);
        cr.paint().ok();
        let gw = self.gutter_width(&a);
        let g = Geom { pango: area.pango_context(), width: (w - gw - PAD * 2.0).max(40.0), height: h };
        let focused = area.has_focus() && self.window.is_active();
        let caret_on = a.caret_on;
        let (top, until) = {
            let v = &a.tab().view;
            (v.top, v.rows.last().map(|r| r.seg.end + 1).unwrap_or(v.top + (1 << 20)))
        };
        let matches = a.visible_matches(top, until.max(top + 1));
        a.with_view(&g, |v, cx| v.paint(cx, cr, gw + PAD, focused, caret_on, &matches));
        if gw > 0.0 {
            let t = a.theme.clone();
            set_color(cr, t.surface);
            cr.rectangle(0.0, 0.0, gw, h);
            let _ = cr.fill();
            let caret_line = {
                let tab = a.tab();
                tab.doc.line_of(tab.view.sel.caret)
            };
            let font = a.style.font.clone();
            let baseline = a.style.baseline;
            let rows = a.tab().view.rows.clone();
            for r in rows.iter() {
                let Some(n) = r.line_no else { continue };
                let l = pango::Layout::new(&area.pango_context());
                l.set_font_description(Some(&font));
                l.set_text(&n.to_string());
                let lw = l.pixel_size().0 as f64;
                let line = l.line_readonly(0);
                set_color(cr, if Some(n - 1) == caret_line { t.gutter_active } else { t.gutter });
                cr.move_to(gw - PAD - lw, r.y + baseline);
                if let Some(line) = line {
                    pangocairo::functions::show_layout_line(cr, &line);
                }
            }
        }
        // where the input method shows its candidates
        if let Some((x, y, cw, ch)) = a.with_view(&g, |v, cx| v.caret_rect(cx)) {
            self.im.set_cursor_location(&gdk::Rectangle::new((x + gw + PAD) as i32, y as i32, cw as i32, ch as i32));
        }
        drop(a);
        glib::idle_add_local_once(|| {
            if let Some(ui) = get_ui() {
                ui.sync_scrollbars();
            }
        });
    }

    /// Closing: tabs whose unsaved text the session can't keep are asked about one at a time; then the session and
    /// the settings are written and the window goes.
    pub fn try_quit(&self) {
        let pending = {
            let Ok(mut a) = self.app.try_borrow_mut() else { return };
            if !a.quitting {
                // (a new try: the session may be writable again)
                a.session_failed = false;
            }
            a.quitting = true;
            if a.tabs.iter().any(|t| t.saving()) {
                return; // waits for the saves (`finish_save` carries on)
            }
            (0..a.tabs.len()).find(|&i| a.tabs[i].doc.is_dirty() && !session::keeps(&a, i)).map(|i| a.tabs[i].id)
        };
        if let Some(tab) = pending {
            self.ask(Ask::CloseUnsaved { tab });
            return;
        }
        let written = self
            .guarded(|a| {
                // (in a test, only with `persist`)
                let ok = session::save(a);
                if self.test.is_none() {
                    self.save_settings(a);
                }
                ok || a.session_failed || !a.tabs.iter().any(|t| t.doc.is_dirty()) || {
                    // The session can't be written (a full or read-only disk): every unsaved tab is asked about.
                    a.session_failed = true;
                    false
                }
            })
            .unwrap_or(true);
        if !written {
            return self.try_quit();
        }
        self.closing.set(true);
        self.window.close();
    }

    fn ask(&self, ask: Ask) {
        if let Some(test) = &self.test {
            test.asked.borrow_mut().push(format!("{ask:?}"));
            let answer = test.answers.borrow_mut().pop_front().unwrap_or_default();
            self.answer(ask, &answer);
            return;
        }
        match ask {
            Ask::Open => {
                #[allow(deprecated)]
                let chooser = gtk4::FileChooserNative::new(Some("Open"), Some(&self.window), gtk4::FileChooserAction::Open, Some("_Open"), Some("_Cancel"));
                #[allow(deprecated)]
                chooser.set_select_multiple(true);
                let keep = keep_until_answered(&chooser);
                #[allow(deprecated)]
                chooser.connect_response(move |c, r| {
                    if r == gtk4::ResponseType::Accept {
                        let files = c.files();
                        let paths: Vec<PathBuf> = (0..files.n_items()).filter_map(|k| files.item(k).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
                        if let Some(ui) = get_ui() {
                            ui.with(|a| a.open_paths(&paths));
                        }
                    }
                    keep.take();
                    c.destroy();
                });
                #[allow(deprecated)]
                chooser.show();
            }
            Ask::SaveAs { tab, close_after } => {
                #[allow(deprecated)]
                let chooser = gtk4::FileChooserNative::new(Some("Save as"), Some(&self.window), gtk4::FileChooserAction::Save, Some("_Save"), Some("_Cancel"));
                if let Some(path) = self.app.borrow().index_of(tab).and_then(|i| self.app.borrow().tabs[i].doc.path.clone()) {
                    #[allow(deprecated)]
                    let _ = chooser.set_file(&gio::File::for_path(&path));
                } else {
                    #[allow(deprecated)]
                    chooser.set_current_name("Untitled.txt");
                }
                let keep = keep_until_answered(&chooser);
                #[allow(deprecated)]
                chooser.connect_response(move |c, r| {
                    if r == gtk4::ResponseType::Accept {
                        if let Some(path) = c.file().and_then(|f| f.path()) {
                            if let Some(ui) = get_ui() {
                                ui.answer(Ask::SaveAs { tab, close_after }, &path.to_string_lossy());
                            }
                        }
                    } else if let Some(ui) = get_ui() {
                        ui.with(|a| a.quitting = false);
                    }
                    keep.take();
                    c.destroy();
                });
                #[allow(deprecated)]
                chooser.show();
            }
            Ask::CloseUnsaved { tab } => {
                let (name, why) = {
                    let a = self.app.borrow();
                    match a.index_of(tab) {
                        Some(i) => (a.tabs[i].title(), session::unkept_reason(&a, i)),
                        None => return,
                    }
                };
                self.dialog(
                    &format!("Save changes to {name}?"),
                    why,
                    &[("Cancel", "cancel"), ("Don't save", "dont"), ("Save", "save")],
                    move |answer| {
                        if let Some(ui) = get_ui() {
                            ui.answer(Ask::CloseUnsaved { tab }, answer);
                        }
                    },
                );
            }
            Ask::Lossy { tab, path, encoding, close_after } => {
                let (title, bad) = {
                    let a = self.app.borrow();
                    match a.index_of(tab) {
                        Some(i) => (a.tabs[i].title(), a.tabs[i].doc.bad_units),
                        None => return,
                    }
                };
                let ask = Ask::Lossy { tab, path, encoding, close_after };
                let done = move |answer: &str| {
                    if let Some(ui) = get_ui() {
                        ui.answer(ask.clone(), answer);
                    }
                };
                if bad > 0 {
                    // A file that wasn't all text in its encoding: what wasn't shows as U+FFFD, and can't be saved back.
                    let what = if bad == 1 { "One place shows".to_string() } else { format!("{bad} places show") };
                    self.dialog(
                        &format!("Parts of {title} weren't {} text", encoding.label()),
                        &format!("{what} \"\u{FFFD}\" where the file had something else, and would be saved that way."),
                        &[("Cancel", "cancel"), ("Save anyway", "damaged")],
                        done,
                    );
                } else {
                    self.dialog(
                        &format!("Some characters in {title} can't be saved as ANSI"),
                        &format!(
                            "In {} they would become \"?\". UTF-8 keeps every character, and nearly every program reads it.",
                            encoding.label()
                        ),
                        &[("Cancel", "cancel"), ("Save as ANSI anyway", "anyway"), ("Save as UTF-8", "utf8")],
                        done,
                    );
                }
            }
            Ask::GotoLine => self.goto_dialog(),
            Ask::Quit => self.try_quit(),
            Ask::About => {
                let about = gtk4::AboutDialog::builder()
                    .transient_for(&self.window)
                    .modal(true)
                    .program_name("Slate")
                    .version(env!("CARGO_PKG_VERSION"))
                    .comments("A fast, simple text editor that opens files of any size.")
                    .website("https://github.com/jamesccupps/Slate")
                    .license_type(gtk4::License::MitX11)
                    .logo_icon_name(APP_ID)
                    .build();
                about.present();
            }
            Ask::MakeDefault => {
                self.dialog(
                    "Open your text files with Slate?",
                    "Slate becomes the app that opens text, Markdown, CSV, JSON, XML, YAML, log and code files when you \
                     double-click them, for your account. Another app can be picked again any time (right-click a \
                     file, Open With).",
                    &[("Cancel", "cancel"), ("Set up", "setup")],
                    |answer| {
                        if let Some(ui) = get_ui() {
                            ui.answer(Ask::MakeDefault, answer);
                        }
                    },
                );
            }
            Ask::StopDefault => {
                self.dialog(
                    "Stop opening files with Slate?",
                    "The kinds of files Slate opens when they're double-clicked go back to the system's own choice. \
                     Slate stays installed; Help → Open files with Slate… sets it up again.",
                    &[("Cancel", "cancel"), ("Stop", "stop")],
                    |answer| {
                        if let Some(ui) = get_ui() {
                            ui.answer(Ask::StopDefault, answer);
                        }
                    },
                );
            }
            Ask::Shortcuts => {
                self.with(|a| {
                    // (the tab that shows them already, if there is one)
                    let i = match a.tabs.iter().position(|t| t.title_override.as_deref() == Some("Keyboard shortcuts")) {
                        Some(i) => i,
                        None => {
                            let i = a.add_text_tab(SHORTCUTS.as_bytes(), None, Some(Lang::Plain));
                            a.tabs[i].doc.mark_saved();
                            a.tabs[i].title_override = Some("Keyboard shortcuts".into());
                            i
                        }
                    };
                    a.activate(i);
                    a.dirty_title = true;
                });
            }
        }
    }

    /// What was answered (by the user, or by a test script).
    fn answer(&self, ask: Ask, answer: &str) {
        match ask {
            Ask::StopDefault => {
                if answer != "stop" {
                    return;
                }
                // (A test only says what it would do, like Open files with Slate…)
                let result = if self.test.is_some() { Ok(0) } else { stop_default() };
                if result.is_ok() {
                    self.is_default.set(false);
                }
                self.with(|a| match result {
                    Ok(0) => a.flash("Slate wasn't opening any kind of file by default.", false),
                    Ok(n) => a.flash(format!("{n} kinds of files open with the system's own choice again."), false),
                    Err(e) => a.flash(e, true),
                });
            }
            Ask::MakeDefault => {
                if answer != "setup" {
                    return;
                }
                // (A test only says what it would do: it mustn't change the defaults of whoever runs it.)
                let result = if self.test.is_some() { Ok(0) } else { make_default() };
                if result.is_ok() {
                    self.is_default.set(self.test.is_some() || slate_is_default());
                }
                self.with(|a| match result {
                    Ok(n) => a.flash(format!("Slate now opens {n} kinds of files when they're double-clicked."), false),
                    Err(e) => a.flash(e, true),
                });
            }
            Ask::Open => {
                if !answer.is_empty() {
                    let p = PathBuf::from(answer);
                    self.with(|a| a.open_paths(&[p]));
                }
            }
            Ask::SaveAs { tab, close_after } => {
                if answer.is_empty() {
                    self.with(|a| a.quitting = false);
                    return;
                }
                let path = PathBuf::from(answer);
                self.with(|a| {
                    if let Some(i) = a.index_of(tab) {
                        let enc = a.tabs[i].doc.encoding;
                        a.start_save(i, path, enc, close_after, false);
                    }
                });
            }
            Ask::CloseUnsaved { tab } => {
                self.with(|a| {
                    let Some(i) = a.index_of(tab) else { return };
                    match answer {
                        "save" => a.exec_save(i, true),
                        "dont" => {
                            a.tabs[i].doc.mark_saved();
                            a.remove_tab(i);
                        }
                        _ => a.quitting = false,
                    }
                });
                let quitting = self.app.try_borrow().is_ok_and(|a| a.quitting);
                if quitting && answer == "dont" {
                    self.try_quit();
                }
            }
            Ask::Lossy { tab, path, encoding, close_after } => {
                self.with(|a| {
                    let Some(i) = a.index_of(tab) else { return };
                    match answer {
                        // (a damaged file, saved as it reads now: closing carries on)
                        "damaged" | "anyway" if a.tabs[i].doc.bad_units > 0 => a.start_save(i, path, encoding, close_after, true),
                        // ANSI with "?" in it: the tab (and the window) stay open, the text still in it
                        "anyway" => {
                            a.quitting = false;
                            a.start_save(i, path, encoding, false, true);
                        }
                        "utf8" => {
                            a.tabs[i].doc.bom = false;
                            a.start_save(i, path, crate::core::text::Encoding::Utf8, close_after, false);
                        }
                        _ => a.quitting = false,
                    }
                });
            }
            Ask::GotoLine => {
                let mut parts = answer.trim().split(':');
                let line = parts.next().and_then(|s| s.replace([',', ' ', '.'], "").parse::<u64>().ok());
                let col = parts.next().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(1);
                if let Some(line) = line {
                    self.with(|a| {
                        let i = a.active;
                        a.tabs[i].goto = Some((line.max(1), col.max(1)));
                        a.goto_now(i);
                    });
                }
            }
            _ => {}
        }
    }

    fn dialog(&self, text: &str, detail: &str, buttons: &[(&str, &'static str)], done: impl Fn(&str) + 'static) {
        #[allow(deprecated)]
        let d = gtk4::MessageDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .message_type(gtk4::MessageType::Question)
            .text(text)
            .secondary_text(detail)
            .build();
        let names: Vec<&'static str> = buttons.iter().map(|b| b.1).collect();
        for (k, (label, _)) in buttons.iter().enumerate() {
            #[allow(deprecated)]
            d.add_button(label, gtk4::ResponseType::Other(k as u16));
        }
        #[allow(deprecated)]
        d.set_default_response(gtk4::ResponseType::Other((buttons.len() - 1) as u16));
        #[allow(deprecated)]
        d.connect_response(move |d, r| {
            let answer = match r {
                gtk4::ResponseType::Other(k) => names.get(k as usize).copied().unwrap_or("cancel"),
                _ => "cancel",
            };
            d.destroy();
            done(answer);
        });
        d.present();
    }

    fn goto_dialog(&self) {
        #[allow(deprecated)]
        let d = gtk4::MessageDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .message_type(gtk4::MessageType::Question)
            .text("Go to line")
            .build();
        let entry = gtk4::Entry::builder().placeholder_text("Line (or line:column)").activates_default(true).build();
        #[allow(deprecated)]
        d.message_area().downcast::<gtk4::Box>().map(|b| b.append(&entry)).ok();
        #[allow(deprecated)]
        d.add_button("Cancel", gtk4::ResponseType::Cancel);
        #[allow(deprecated)]
        d.add_button("Go", gtk4::ResponseType::Ok);
        #[allow(deprecated)]
        d.set_default_response(gtk4::ResponseType::Ok);
        #[allow(deprecated)]
        d.connect_response(move |d, r| {
            let text = entry.text().to_string();
            d.destroy();
            if r == gtk4::ResponseType::Ok {
                if let Some(ui) = get_ui() {
                    ui.answer(Ask::GotoLine, &text);
                    ui.text.grab_focus();
                }
            }
        });
        d.present();
    }
}
