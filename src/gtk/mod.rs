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
    pub test: Option<Rc<testmode::Test>>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

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

pub fn run(args: Vec<String>) -> i32 {
    if args.first().map(String::as_str) == Some("--test") {
        return testmode::run(&args[1..]);
    }
    let application = gtk4::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
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
    let argv: Vec<String> = std::iter::once("slate".to_string()).chain(args).collect();
    if application.run_with_args(&argv) == glib::ExitCode::SUCCESS { 0 } else { 1 }
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
    if gs.is_gtk_application_prefer_dark_theme() {
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
    let notice = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    notice.add_css_class("slate-notice");
    notice.append(&notice_label);
    notice.append(&notice_close);
    notice.set_visible(false);

    let menubar = gtk4::PopoverMenuBar::from_model(Some(&menu_model()));
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
    session::restore(&mut app);
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
        test,
    });
    UI.with(|u| *u.borrow_mut() = Some(ui.clone()));
    connect(&ui, &prev, &next, &close_find, &replace_one, &replace_all, &notice_close);
    actions(&ui);
    // timers: the caret, files changed by other programs, the session
    glib::timeout_add_local(Duration::from_millis(BLINK_MS), || {
        if let Some(ui) = get_ui() {
            ui.blink();
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_local(Duration::from_secs(2), || {
        if let Some(ui) = get_ui() {
            if ui.window.is_active() {
                ui.with(|a| a.check_disk());
            }
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_local(Duration::from_secs(5), || {
        if let Some(ui) = get_ui() {
            let dirty = ui.app.try_borrow().is_ok_and(|a| a.session_dirty);
            if dirty && ui.test.is_none() {
                if let Ok(mut a) = ui.app.try_borrow_mut() {
                    session::save(&mut a);
                }
            }
        }
        glib::ControlFlow::Continue
    });
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
    ui.tabs.set_draw_func(|area, cr, w, h| {
        if let Some(ui) = get_ui() {
            let Ok(app) = ui.app.try_borrow() else { return };
            let mut first = ui.first_tab.get();
            let strip = chrome::tab_strip(&app, w as f64, &mut first);
            ui.first_tab.set(first);
            chrome::paint_tabs(cr, &area.pango_context(), &app, &strip, h as f64, ui.hover.get());
            *ui.strip.borrow_mut() = strip;
        }
    });
    ui.status.set_draw_func(|area, cr, w, h| {
        if let Some(ui) = get_ui() {
            let Ok(app) = ui.app.try_borrow() else { return };
            let items = chrome::paint_status(cr, &area.pango_context(), &app, w as f64, h as f64);
            *ui.status_items.borrow_mut() = items;
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
    notice_close.connect_clicked(|_| {
        if let Some(ui) = get_ui() {
            ui.with(|a| a.tab_mut().notice = None);
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
        ("json-format", Cmd::JsonFormat, &["<Shift><Alt>f"]),
        ("json-minify", Cmd::JsonMinify, &[]),
        ("json-check", Cmd::JsonCheck, &[]),
        ("xml-format", Cmd::XmlFormat, &[]),
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
    ]
}

fn actions(ui: &Rc<Ui>) {
    for (name, cmd, accels) in commands() {
        let a = gio::SimpleAction::new(name, None);
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
        ui.gtk_app.set_accels_for_action(&format!("win.{name}"), &[&format!("<Alt>{k}")]);
    }
    let lang = gio::SimpleAction::new("set-lang", Some(glib::VariantTy::STRING));
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

fn section(items: &[(&str, &str)]) -> gio::Menu {
    let m = gio::Menu::new();
    for (label, action) in items {
        m.append(Some(label), Some(&format!("win.{action}")));
    }
    m
}

fn menu_model() -> gio::Menu {
    let bar = gio::Menu::new();
    let file = gio::Menu::new();
    file.append_section(None, &section(&[("New tab", "new-tab"), ("Open…", "open")]));
    file.append_section(None, &section(&[("Save", "save"), ("Save as…", "save-as"), ("Save all", "save-all")]));
    file.append_section(None, &section(&[("Close tab", "close-tab"), ("Reopen closed tab", "reopen-closed"), ("Quit", "quit")]));
    bar.append_submenu(Some("_File"), &file);
    let edit = gio::Menu::new();
    edit.append_section(None, &section(&[("Undo", "undo"), ("Redo", "redo")]));
    edit.append_section(None, &section(&[("Cut", "cut"), ("Copy", "copy"), ("Paste", "paste"), ("Select all", "select-all")]));
    edit.append_section(None, &section(&[("Find…", "find"), ("Replace…", "replace"), ("Find next", "find-next"), ("Find previous", "find-prev"), ("Go to line…", "goto-line")]));
    edit.append_section(None, &section(&[("Comment / uncomment", "toggle-comment"), ("Duplicate line", "duplicate-line"), ("Delete line", "delete-line")]));
    let lines = section(&[("Sort A to Z", "sort-asc"), ("Sort Z to A", "sort-desc"), ("Remove duplicate lines", "dedupe"), ("Remove blank lines", "remove-blank"), ("Trim spaces at line ends", "trim")]);
    edit.append_submenu(Some("Lines"), &lines);
    let case = section(&[("UPPERCASE", "upper"), ("lowercase", "lower"), ("Title Case", "title-case")]);
    edit.append_submenu(Some("Change case"), &case);
    edit.append_section(None, &section(&[("Insert time and date", "date-time")]));
    bar.append_submenu(Some("_Edit"), &edit);
    let view = gio::Menu::new();
    view.append_section(None, &section(&[("Word wrap", "wrap"), ("Line numbers", "line-numbers")]));
    view.append_section(None, &section(&[("Zoom in", "zoom-in"), ("Zoom out", "zoom-out"), ("Reset zoom", "zoom-reset")]));
    let theme = section(&[("Like the system", "theme-system"), ("Light", "theme-light"), ("Dark", "theme-dark")]);
    view.append_submenu(Some("Theme"), &theme);
    bar.append_submenu(Some("_View"), &view);
    let format = gio::Menu::new();
    format.append_section(None, &section(&[("Format JSON", "json-format"), ("Minify JSON", "json-minify"), ("Check JSON", "json-check")]));
    format.append_section(None, &section(&[("Format XML", "xml-format"), ("Check XML", "xml-check")]));
    let eol = section(&[("Unix (LF)", "eol-lf"), ("Windows (CRLF)", "eol-crlf")]);
    format.append_submenu(Some("Line breaks"), &eol);
    format.append_submenu(Some("Language"), &lang_menu());
    bar.append_submenu(Some("F_ormat"), &format);
    let help = gio::Menu::new();
    help.append_section(None, &section(&[("Keyboard shortcuts", "shortcuts"), ("About Slate", "about")]));
    bar.append_submenu(Some("_Help"), &help);
    bar
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
  Alt+1 … Alt+9            Go to a tab (9: the last)
  Ctrl+Q                   Quit

  Ctrl+Z / Ctrl+Y          Undo / redo
  Ctrl+X, C, V, A          Cut, copy, paste, select all (with nothing selected: the line)
  Ctrl+F / Ctrl+H          Find / replace
  F3 / Shift+F3            Next / previous match
  Ctrl+G                   Go to line
  Ctrl+/                   Comment / uncomment the lines
  Ctrl+D / Ctrl+Shift+K    Duplicate / delete the line
  Alt+Up / Alt+Down        Move the line up / down
  Tab / Shift+Tab          Indent / outdent (with lines selected)
  Ctrl+U / Ctrl+Shift+U    lowercase / UPPERCASE
  Shift+Alt+F              Format JSON
  F5                       Insert the time and date

  Alt+Z                    Word wrap
  Ctrl+Plus / Ctrl+Minus   Zoom in / out (Ctrl+0: reset)
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
        let r = match self.app.try_borrow_mut() {
            Ok(mut a) => f(&mut a),
            Err(_) => return None,
        };
        self.refresh();
        Some(r)
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
            Cmd::CloseFind => {
                self.text.grab_focus();
            }
            _ => {}
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
        self.touch();
        let g = self.geom();
        let handled = match self.app.try_borrow_mut() {
            Ok(mut a) => a.on_key(&g, keyval, state),
            Err(_) => false,
        };
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
            2 => section(&[("Unix (LF)", "eol-lf"), ("Windows (CRLF)", "eol-crlf")]),
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
        let running = match self.app.try_borrow_mut() {
            Ok(mut a) => a.poll_jobs(),
            Err(_) => true,
        };
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
            }
            None => self.notice.set_visible(false),
        }
        self.hbar.set_visible(!a.style.wrap);
        self.syncing.set(false);
        let dark = a.theme.dark;
        drop(a);
        if self.css_dark.get() != Some(dark) {
            self.apply_theme();
        }
        self.queue_all();
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
            let Ok(a) = self.app.try_borrow() else { return };
            if a.tabs.iter().any(|t| t.saving()) {
                return; // waits for the saves (`poll` asks again)
            }
            (0..a.tabs.len()).find(|&i| a.tabs[i].doc.is_dirty() && !session::keeps(&a, i))
        };
        if let Some(i) = pending {
            let id = self.app.borrow().tabs[i].id;
            if let Ok(mut a) = self.app.try_borrow_mut() {
                a.quitting = true;
            }
            self.ask(Ask::CloseUnsaved { tab: id });
            return;
        }
        if let Ok(mut a) = self.app.try_borrow_mut() {
            if self.test.is_none() {
                session::save(&mut a);
                let (w, h) = self.window.default_size();
                a.settings.window = Some(crate::settings::Placement { x: 0, y: 0, w, h, maximized: self.window.is_maximized() });
                if crate::settings::persist() {
                    a.settings.save();
                }
            }
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
                #[allow(deprecated)]
                chooser.connect_response(|c, r| {
                    if r == gtk4::ResponseType::Accept {
                        let files = c.files();
                        let paths: Vec<PathBuf> = (0..files.n_items()).filter_map(|k| files.item(k).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
                        if let Some(ui) = get_ui() {
                            ui.with(|a| a.open_paths(&paths));
                        }
                    }
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
                    c.destroy();
                });
                #[allow(deprecated)]
                chooser.show();
            }
            Ask::CloseUnsaved { tab } => {
                let name = self.app.borrow().index_of(tab).map(|i| self.app.borrow().tabs[i].title()).unwrap_or_default();
                self.dialog(
                    &format!("Save changes to {name}?"),
                    "Your changes will be lost if you don't save them.",
                    &[("Cancel", "cancel"), ("Don't save", "dont"), ("Save", "save")],
                    move |answer| {
                        if let Some(ui) = get_ui() {
                            ui.answer(Ask::CloseUnsaved { tab }, answer);
                        }
                    },
                );
            }
            Ask::Lossy { tab, path, encoding, close_after } => {
                let ask = Ask::Lossy { tab, path, encoding, close_after };
                self.dialog(
                    "Some characters can't be saved in this encoding",
                    "Saving anyway turns them into \"?\" (or keeps a damaged file's broken parts as they read now).",
                    &[("Cancel", "cancel"), ("Save anyway", "anyway"), ("Save as UTF-8", "utf8")],
                    move |answer| {
                        if let Some(ui) = get_ui() {
                            ui.answer(ask.clone(), answer);
                        }
                    },
                );
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
            Ask::Shortcuts => {
                self.with(|a| {
                    let i = a.add_text_tab(SHORTCUTS.as_bytes(), None, Some(Lang::Plain));
                    a.tabs[i].doc.mark_saved();
                    a.active = i;
                    a.dirty_title = true;
                });
            }
        }
    }

    /// What was answered (by the user, or by a test script).
    fn answer(&self, ask: Ask, answer: &str) {
        match ask {
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
                        "anyway" => a.start_save(i, path, encoding, close_after, true),
                        "utf8" => a.start_save(i, path, crate::core::text::Encoding::Utf8, close_after, false),
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
