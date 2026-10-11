//! `slate --test script.txt` (or the commands as arguments): drives the real window and renders it into PNG
//! files, so the Linux version can be checked without anyone at the screen (under Xvfb, say). Prompts are never
//! shown (they take answers from `answer:` lines and are listed by `print:asked`), the clipboard is a private one,
//! and settings and the session aren't written unless `persist` (which needs `SLATE_DATA_DIR`).
//!
//! Commands: `size:1000x700`, `theme:dark|light`, `open:<path>`, `type:<text>` (`\n`, `\t` allowed), `key:<combo>`
//! (`ctrl+shift+k`, `enter`, `pagedown`, `f3`), `cmd:<action>` (a menu action's name: `save`, `json-format`...),
//! `find:<text>`, `replace:<text>`, `goto:<line>`, `answer:save,dont,cancel` (or a path for Open and Save as),
//! `lang:<name>`, `set:wrap=true` (also `line_numbers`, `font_size`), `wait:<ms>`, `jobs` (wait for background
//! work), `shot:<file.png>`, `print:<what>` (`text`, `sel`, `status`, `title`, `tabs`, `lang`, `asked`,
//! `clipboard`, `find`, `dirty`, `top`, `session`: the session folder's files, `notice`), `expect:<what>=<value>`,
//! `t:<label>` (a timing mark), `persist`, `session:save|soon|restore` (`soon`: on another thread, as the timer
//! does), `quit`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, graphene};

use super::app::Cmd;
use super::{commands, get_ui, session, window};

#[derive(Default)]
pub struct Test {
    pub answers: RefCell<VecDeque<String>>,
    pub asked: RefCell<Vec<String>>,
    pub clipboard: RefCell<String>,
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r")
}

/// A key combo (`ctrl+shift+k`) as GDK's key and modifiers, and as an accelerator (`<Control><Shift>k`).
fn combo(s: &str) -> Option<(gdk::Key, gdk::ModifierType, String)> {
    let mut mods = gdk::ModifierType::empty();
    let mut accel = String::new();
    let mut key = None;
    for part in s.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                mods |= gdk::ModifierType::CONTROL_MASK;
                accel.push_str("<Control>");
            }
            "shift" => {
                mods |= gdk::ModifierType::SHIFT_MASK;
                accel.push_str("<Shift>");
            }
            "alt" => {
                mods |= gdk::ModifierType::ALT_MASK;
                accel.push_str("<Alt>");
            }
            k => {
                let name = match k {
                    "enter" | "return" => "Return",
                    "esc" | "escape" => "Escape",
                    "tab" => "Tab",
                    "backspace" => "BackSpace",
                    "delete" | "del" => "Delete",
                    "left" => "Left",
                    "right" => "Right",
                    "up" => "Up",
                    "down" => "Down",
                    "home" => "Home",
                    "end" => "End",
                    "pageup" => "Page_Up",
                    "pagedown" => "Page_Down",
                    "space" => "space",
                    "slash" | "/" => "slash",
                    "plus" => "plus",
                    "minus" => "minus",
                    f if f.starts_with('f') && f.len() > 1 && f[1..].parse::<u8>().is_ok() => {
                        key = gdk::Key::from_name(f.to_ascii_uppercase());
                        accel.push_str(&f.to_ascii_uppercase());
                        continue;
                    }
                    other => other,
                };
                key = gdk::Key::from_name(name);
                accel.push_str(name);
            }
        }
    }
    key.map(|k| (k, mods, accel))
}

pub fn run(args: &[String]) -> i32 {
    let lines: Vec<String> = if args.len() == 1 && std::path::Path::new(&args[0]).is_file() {
        std::fs::read_to_string(&args[0]).unwrap_or_default().lines().map(String::from).collect()
    } else {
        args.to_vec()
    };
    let persist = lines.iter().any(|l| l.trim() == "persist") && std::env::var_os("SLATE_DATA_DIR").is_some();
    crate::settings::NO_PERSIST.store(!persist, std::sync::atomic::Ordering::Relaxed);
    let id = format!("{}.Test{}", super::APP_ID, std::process::id());
    let application = gtk4::Application::builder().application_id(id).flags(gio::ApplicationFlags::NON_UNIQUE).build();
    let failures = Rc::new(std::cell::Cell::new(0));
    let f2 = failures.clone();
    application.connect_activate(move |a| {
        let test = Rc::new(Test::default());
        let ui = window(a, Some(test.clone()));
        ui.window.present();
        let lines = lines.clone();
        let failures = f2.clone();
        let a = a.clone();
        glib::spawn_future_local(async move {
            let n = script(lines, test).await;
            failures.set(n);
            if let Some(ui) = get_ui() {
                ui.closing_now();
            }
            a.quit();
        });
    });
    application.run_with_args::<&str>(&[]);
    failures.get()
}

fn log(out: &mut Vec<String>, s: String) {
    println!("{s}");
    out.push(s);
}

async fn settle(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

async fn jobs() {
    let start = Instant::now();
    loop {
        settle(15).await;
        let Some(ui) = get_ui() else { return };
        let busy = ui.app.try_borrow().map(|a| a.busy()).unwrap_or(true);
        if !busy || start.elapsed() > Duration::from_secs(120) {
            ui.refresh();
            return;
        }
    }
}

/// Runs the script; returns how many expectations failed.
async fn script(lines: Vec<String>, test: Rc<Test>) -> i32 {
    let mut out = Vec::new();
    let mut failures = 0;
    let started = Instant::now();
    let mut mark = started;
    settle(80).await;
    for line in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some(ui) = get_ui() else { break };
        let (op, arg) = line.split_once(':').unwrap_or((line, ""));
        match op {
            "size" => {
                if let Some((w, h)) = arg.split_once('x') {
                    ui.window.set_default_size(w.parse().unwrap_or(1000), h.parse().unwrap_or(700));
                    settle(120).await;
                }
            }
            "theme" => ui.command(Cmd::Theme(if arg == "dark" { crate::settings::ThemeMode::Dark } else { crate::settings::ThemeMode::Light })),
            "open" => {
                let p = PathBuf::from(arg);
                ui.with(|a| a.open_paths(&[p]));
                settle(10).await;
            }
            "type" => {
                for c in unescape(arg).chars() {
                    let mut b = [0u8; 4];
                    let s = c.encode_utf8(&mut b).to_string();
                    if c == '\n' {
                        let _ = ui.key_for_test(gdk::Key::Return, gdk::ModifierType::empty());
                    } else if c == '\t' {
                        let _ = ui.key_for_test(gdk::Key::Tab, gdk::ModifierType::empty());
                    } else {
                        ui.with(|a| a.insert_text(&s));
                    }
                }
            }
            "key" => match combo(arg) {
                Some((k, m, accel)) => {
                    if !ui.key_for_test(k, m) {
                        match commands().into_iter().find(|(_, _, accels)| accels.iter().any(|a| a.eq_ignore_ascii_case(&accel))) {
                            Some((_, cmd, _)) => ui.command(cmd),
                            None => log(&mut out, format!("key {arg}: nothing does that")),
                        }
                    }
                }
                None => {
                    log(&mut out, format!("unknown key {arg}"));
                    failures += 1;
                }
            },
            "cmd" => match commands().into_iter().find(|(n, _, _)| *n == arg) {
                Some((_, cmd, _)) => ui.command(cmd),
                None => {
                    log(&mut out, format!("unknown command {arg}"));
                    failures += 1;
                }
            },
            "find" => {
                ui.command(Cmd::Find);
                ui.find_entry_for_test().set_text(&unescape(arg));
            }
            "replace" => {
                ui.command(Cmd::Replace);
                ui.replace_entry_for_test().set_text(&unescape(arg));
            }
            "goto" => {
                test.answers.borrow_mut().push_front(arg.to_string());
                ui.command(Cmd::GotoLine);
            }
            "answer" => {
                let mut a = test.answers.borrow_mut();
                for x in arg.split(',') {
                    a.push_back(x.trim().to_string());
                }
            }
            "lang" => match crate::highlight::Lang::ALL.iter().find(|l| l.label().eq_ignore_ascii_case(arg)) {
                Some(&l) => ui.command(Cmd::SetLang(l)),
                None => {
                    log(&mut out, format!("unknown language {arg}"));
                    failures += 1;
                }
            },
            "set" => {
                let (k, v) = arg.split_once('=').unwrap_or((arg, ""));
                ui.with(|a| {
                    match k {
                        "wrap" => {
                            a.settings.wrap = v == "true";
                            a.style.wrap = a.settings.wrap;
                        }
                        "line_numbers" => {
                            a.settings.line_numbers = v == "true";
                            a.style.line_numbers = a.settings.line_numbers;
                        }
                        "font_size" => {
                            a.settings.font_size = v.parse().unwrap_or(11.0);
                            a.restyle = true;
                        }
                        _ => {}
                    }
                    a.dirty_view = true;
                });
                settle(20).await;
            }
            "wait" => settle(arg.parse().unwrap_or(100)).await,
            "jobs" => jobs().await,
            "shot" => {
                // (a window that's being resized or mapped draws nothing for a moment: tried again then)
                let mut r = Err(String::new());
                for _ in 0..20 {
                    settle(30).await;
                    r = shot(arg);
                    if r.is_ok() {
                        break;
                    }
                }
                if let Err(e) = r {
                    log(&mut out, format!("shot {arg}: {e}"));
                    failures += 1;
                }
            }
            "print" => {
                let v = value(arg, &test);
                log(&mut out, format!("{arg}: {v}"));
            }
            "expect" => {
                let (k, want) = arg.split_once('=').unwrap_or((arg, ""));
                let got = value(k, &test);
                if got == unescape(want) {
                    log(&mut out, format!("ok {k}"));
                } else {
                    log(&mut out, format!("FAIL {k}: expected {:?}, got {:?}", unescape(want), got));
                    failures += 1;
                }
            }
            "t" => {
                let now = Instant::now();
                log(&mut out, format!("[{:8.1} ms | +{:8.1} ms] {arg}", (now - started).as_secs_f64() * 1000.0, (now - mark).as_secs_f64() * 1000.0));
                mark = now;
            }
            "persist" => {}
            "session" => {
                if std::env::var_os("SLATE_DATA_DIR").is_none() {
                    log(&mut out, format!("session:{arg} needs SLATE_DATA_DIR"));
                    failures += 1;
                } else if arg == "save" {
                    let ok = ui.app.try_borrow_mut().map(|mut a| session::save(&mut a)).unwrap_or(false);
                    if !ok {
                        log(&mut out, "session: not all written".into());
                    }
                } else if arg == "soon" {
                    // (as the timer does: on another thread)
                    ui.with(|a| {
                        let notify = a.notify.clone();
                        session::start(a, notify);
                    });
                } else if arg == "restore" {
                    ui.with(|a| {
                        a.tabs.clear();
                        session::restore(a, false);
                        if a.tabs.is_empty() {
                            a.new_untitled();
                        }
                    });
                }
            }
            "quit" => {
                ui.try_quit();
                settle(50).await;
            }
            other => {
                log(&mut out, format!("unknown command {other}"));
                failures += 1;
            }
        }
        // (let the window draw and lay out between commands)
        settle(5).await;
    }
    if let Some(p) = std::env::var_os("SLATE_TEST_LOG") {
        if let Ok(mut f) = std::fs::File::create(p) {
            for l in &out {
                let _ = writeln!(f, "{l}");
            }
        }
    }
    failures
}

fn value(what: &str, test: &Test) -> String {
    let Some(ui) = get_ui() else { return String::new() };
    let Ok(a) = ui.app.try_borrow() else { return "(busy)".into() };
    let tab = a.tab();
    match what {
        "text" => String::from_utf8_lossy(&tab.doc.read(0, tab.doc.len().min(1 << 20))).into_owned(),
        "sel" => format!("{}..{}", tab.view.sel.anchor, tab.view.sel.caret),
        "status" => super::chrome::status_texts(&a).0,
        "statusbar" => {
            let (l, r, _) = super::chrome::status_texts(&a);
            format!("{l} | {}", r.join(" | "))
        }
        "title" => ui.window.title().map(|t| t.to_string()).unwrap_or_default(),
        "tabs" => a.tabs.len().to_string(),
        "tabnames" => a.tabs.iter().map(|t| t.title()).collect::<Vec<_>>().join(", "),
        "lang" => tab.lang.label().to_string(),
        "asked" => test.asked.borrow().join("; "),
        "clipboard" => test.clipboard.borrow().clone(),
        "find" => format!("{:?}", a.find.found.as_ref().map(|f| f.1.count)),
        "dirty" => tab.doc.is_dirty().to_string(),
        "top" => format!("{}+{}", tab.view.top, tab.view.top_row),
        "wrap" => a.settings.wrap.to_string(),
        "theme" => if a.theme.dark { "dark" } else { "light" }.to_string(),
        // the files in the session folder
        "session" => {
            let mut v: Vec<String> = std::fs::read_dir(crate::settings::data_dir().join("session"))
                .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            v.sort();
            v.join(" ")
        }
        "notice" => tab.notice.clone().unwrap_or_default(),
        other => format!("(unknown: {other})"),
    }
}

/// Renders the window into a PNG.
fn shot(path: &str) -> Result<(), String> {
    let ui = get_ui().ok_or("no window")?;
    let w = ui.window.width().max(1) as f64;
    let h = ui.window.height().max(1) as f64;
    let paintable = gtk4::WidgetPaintable::new(Some(&ui.window));
    let snap = gtk4::Snapshot::new();
    paintable.snapshot(&snap, w, h);
    let node = snap.to_node().ok_or("nothing drawn")?;
    let renderer = ui.window.native().and_then(|n| n.renderer()).ok_or("no renderer")?;
    let tex = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    tex.save_to_png(path).map_err(|e| e.to_string())
}
