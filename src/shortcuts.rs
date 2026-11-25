//! Keyboard shortcuts the user can change (Settings → Change keyboard shortcuts…), shared by both apps: every command's
//! name and the keys it has, and the user's file (`shortcuts.txt` in the data folder) read over them.
//!
//! The file lists every command, each line commented out with the keys it has (`# duplicate-line = Ctrl+D`); a line
//! without the `#` gives that command its own keys instead (`duplicate-line = Ctrl+Shift+D`, or none:
//! `duplicate-line =`), and those keys stop meaning anything else. Each app keeps its own way of matching its keys
//! (Windows `commands::global_key` / `editor_key`, GTK's accelerators) and lays the user's over them (`Keymap`); the
//! keys for moving and typing (arrows, Home, Backspace, Ctrl+1–9…) stay as they are.

use std::collections::HashMap;
use std::path::PathBuf;

const WIN: bool = cfg!(windows);

/// A key with Ctrl, Alt and Shift held or not.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    /// A letter (upper case), a digit, or one of / \ [ ] ; ' , . - = ` ?
    Char(u8),
    /// F1–F24.
    F(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Backspace,
    Enter,
    Tab,
    Esc,
    Space,
    /// The number pad's +, -, / and digits.
    NumPlus,
    NumMinus,
    NumSlash,
    Num(u8),
}

const SYMBOLS: &[u8] = b"/\\[];',.-=`?";

/// Names keys go by besides their own (`Key::name`).
const ALIASES: &[(&str, Key)] = &[
    ("pgup", Key::PageUp),
    ("page_up", Key::PageUp),
    ("pgdn", Key::PageDown),
    ("page_down", Key::PageDown),
    ("ins", Key::Insert),
    ("del", Key::Delete),
    ("return", Key::Enter),
    ("escape", Key::Esc),
    ("plus", Key::Char(b'=')),
    ("minus", Key::Char(b'-')),
    ("slash", Key::Char(b'/')),
    ("backslash", Key::Char(b'\\')),
    ("comma", Key::Char(b',')),
    ("period", Key::Char(b'.')),
];

impl Key {
    pub fn name(self) -> String {
        match self {
            Key::Char(c) => (c as char).to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Num(n) => format!("Num{n}"),
            k => format!("{k:?}"),
        }
    }

    fn parse(s: &str) -> Option<Key> {
        let l = s.to_ascii_lowercase();
        if s.len() == 1 {
            let c = s.as_bytes()[0].to_ascii_uppercase();
            return (c.is_ascii_alphanumeric() || SYMBOLS.contains(&c)).then_some(Key::Char(c));
        }
        if let Some(n) = l.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            return (1..=24).contains(&n).then_some(Key::F(n));
        }
        if let Some(n) = l.strip_prefix("num").and_then(|n| n.parse::<u8>().ok()) {
            return (n <= 9).then_some(Key::Num(n));
        }
        let named = [
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Insert,
            Key::Delete,
            Key::Backspace,
            Key::Enter,
            Key::Tab,
            Key::Esc,
            Key::Space,
            Key::NumPlus,
            Key::NumMinus,
            Key::NumSlash,
        ];
        named.into_iter().find(|k| k.name().eq_ignore_ascii_case(&l)).or_else(|| ALIASES.iter().find(|a| a.0 == l).map(|a| a.1))
    }
}

impl Combo {
    /// "Ctrl+Shift+D", "Shift+Alt+F", "F3" (any case; Control, Return, Escape, PgUp… are understood too).
    pub fn parse(s: &str) -> Option<Combo> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last()?;
        let mut c = Combo { ctrl: false, alt: false, shift: false, key: Key::parse(key)? };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "ctl" => c.ctrl = true,
                "alt" => c.alt = true,
                "shift" => c.shift = true,
                _ => return None,
            }
        }
        // (Windows tells keys by where they are, not by what they type: ? is Shift and the / key)
        if cfg!(windows) && c.key == Key::Char(b'?') {
            c.key = Key::Char(b'/');
            c.shift = true;
        }
        Some(c)
    }

    /// Typing stays typing: a shortcut has Ctrl or Alt, or is an F key.
    pub fn usable(&self) -> bool {
        self.ctrl || self.alt || matches!(self.key, Key::F(_))
    }

    /// The menus' way: Ctrl, Shift, Alt, then the key.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.shift, "Shift+"), (self.alt, "Alt+")] {
            if on {
                s.push_str(name);
            }
        }
        s + &self.key.name()
    }

    /// As GTK writes accelerators: `<Control><Shift>d`.
    pub fn gtk(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "<Control>"), (self.shift, "<Shift>"), (self.alt, "<Alt>")] {
            if on {
                s.push_str(name);
            }
        }
        let key = match self.key {
            Key::Char(c) if c.is_ascii_alphanumeric() => (c.to_ascii_lowercase() as char).to_string(),
            Key::Char(c) => match c {
                b'/' => "slash",
                b'\\' => "backslash",
                b'[' => "bracketleft",
                b']' => "bracketright",
                b';' => "semicolon",
                b'\'' => "apostrophe",
                b',' => "comma",
                b'.' => "period",
                b'-' => "minus",
                b'=' => "equal",
                b'`' => "grave",
                _ => "question",
            }
            .into(),
            Key::F(n) => format!("F{n}"),
            Key::PageUp => "Page_Up".into(),
            Key::PageDown => "Page_Down".into(),
            Key::Backspace => "BackSpace".into(),
            Key::Enter => "Return".into(),
            Key::Esc => "Escape".into(),
            Key::Space => "space".into(),
            Key::NumPlus => "KP_Add".into(),
            Key::NumMinus => "KP_Subtract".into(),
            Key::NumSlash => "KP_Divide".into(),
            Key::Num(n) => format!("KP_{n}"),
            k => format!("{k:?}"),
        };
        s + &key
    }
}

/// A command whose keys can be changed.
pub struct Command {
    /// Its name in the file (the GTK app's action name).
    pub id: &'static str,
    /// The keys it has unless the file says otherwise ("Ctrl+N, Ctrl+T"; empty: none).
    pub keys: &'static str,
    /// Windows: works in the find bar's boxes too (else in the text only, as editing does).
    pub anywhere: bool,
    /// The app it's in: both (None), Windows only (Some(true)), Linux only (Some(false)).
    only: Option<bool>,
    /// The heading it's listed under in the file.
    pub group: &'static str,
}

const fn any(group: &'static str, id: &'static str, keys: &'static str) -> Command {
    Command { id, keys, anywhere: true, only: None, group }
}

const fn text(group: &'static str, id: &'static str, keys: &'static str) -> Command {
    Command { id, keys, anywhere: false, only: None, group }
}

const fn only(c: Command, windows: bool) -> Command {
    Command { only: Some(windows), ..c }
}

const F: &str = "File";
const E: &str = "Edit";
const V: &str = "View";
const O: &str = "Format";
const H: &str = "Help";

const ALL: &[Command] = &[
    any(F, "new-tab", "Ctrl+N, Ctrl+T"),
    any(F, "open", "Ctrl+O"),
    text(F, "open-selection", ""),
    any(F, "reopen-where-left", ""),
    any(F, "reopen-closed", "Ctrl+Shift+T"),
    any(F, "save", "Ctrl+S"),
    any(F, "save-as", "Ctrl+Shift+S"),
    any(F, "save-all", "Ctrl+Alt+S"),
    any(F, "reload", ""),
    any(F, "print", "Ctrl+P"),
    any(F, "print-preview", ""),
    any(F, "page-setup", ""),
    any(F, "close-tab", if WIN { "Ctrl+W, Ctrl+F4" } else { "Ctrl+W" }),
    any(F, "close-others", ""),
    any(F, "close-right", ""),
    any(F, "close-saved", ""),
    any(F, "close-all", ""),
    any(F, "copy-path", ""),
    any(F, "reveal-file", ""),
    any(F, "read-only", ""),
    any(F, "restore-session", ""),
    any(F, "clear-recent", ""),
    // (Windows' own Alt+F4 closes the window)
    any(F, "quit", if WIN { "" } else { "Ctrl+Q" }),
    text(E, "undo", "Ctrl+Z"),
    text(E, "redo", "Ctrl+Y, Ctrl+Shift+Z"),
    text(E, "cut", if WIN { "Ctrl+X, Shift+Delete" } else { "Ctrl+X" }),
    text(E, "copy", if WIN { "Ctrl+C, Ctrl+Insert" } else { "Ctrl+C" }),
    text(E, "paste", if WIN { "Ctrl+V, Shift+Insert" } else { "Ctrl+V" }),
    text(E, "select-all", "Ctrl+A"),
    any(E, "find", "Ctrl+F"),
    any(E, "replace", "Ctrl+H"),
    any(E, "find-next", "F3"),
    any(E, "find-prev", "Shift+F3"),
    any(E, "goto-line", "Ctrl+G"),
    text(E, "goto-bracket", "Ctrl+M"),
    text(E, "select-brackets", "Ctrl+Shift+M"),
    any(E, "toggle-bookmark", "Ctrl+F2"),
    any(E, "next-bookmark", "F2"),
    any(E, "prev-bookmark", "Shift+F2"),
    any(E, "clear-bookmarks", ""),
    text(E, "toggle-comment", if WIN { "Ctrl+/, Ctrl+NumSlash" } else { "Ctrl+/" }),
    text(E, "duplicate-line", "Ctrl+D"),
    text(E, "delete-line", "Ctrl+Shift+K"),
    text(E, "move-line-up", "Alt+Up"),
    text(E, "move-line-down", "Alt+Down"),
    text(E, "sort-asc", ""),
    text(E, "sort-desc", ""),
    text(E, "dedupe", ""),
    text(E, "remove-blank", ""),
    text(E, "trim", ""),
    text(E, "upper", "Ctrl+Shift+U"),
    text(E, "lower", "Ctrl+U"),
    text(E, "title-case", ""),
    text(E, "date-time", "F5"),
    any(E, "find-in-tabs", ""),
    any(E, "replace-in-tabs", ""),
    any(E, "find-in-files", "Ctrl+Shift+F"),
    text(E, "go-back", "Alt+Left"),
    text(E, "go-forward", "Alt+Right"),
    any(E, "overtype", ""),
    any(E, "multi-cursor", ""),
    // (not Ctrl+Alt+Up/Down: GNOME switches workspaces with them, and some Windows graphics drivers turn the screen)
    text(E, "add-cursor-above", "Ctrl+Shift+Up"),
    text(E, "add-cursor-below", "Ctrl+Shift+Down"),
    text(E, "add-next-match", "Ctrl+Shift+N"),
    text(E, "skip-match", ""),
    text(E, "all-matches", "Ctrl+Shift+A"),
    text(E, "cursors-at-line-starts", "Alt+["),
    text(E, "cursors-at-line-ends", "Alt+]"),
    text(E, "remove-last-cursor", ""),
    any(V, "wrap", "Alt+Z"),
    any(V, "line-numbers", ""),
    any(V, "show-whitespace", ""),
    any(V, "hide-menu-bar", ""),
    any(V, "spell-check", ""),
    text(V, "next-misspelling", "F7"),
    any(V, "title-bar-tabs", ""),
    any(V, "close-with-last-tab", ""),
    any(V, "find-close-on-leave", ""),
    any(V, "find-per-tab", ""),
    any(V, "bars-at-bottom", ""),
    any(V, "char-info", ""),
    any(V, "filter-lines", "Alt+L"),
    any(V, "filter-lines-not", ""),
    any(V, "clear-highlights", ""),
    any(V, "structure-panel", "Ctrl+Shift+O"),
    any(V, "path-bar", ""),
    any(V, "zoom-in", if WIN { "Ctrl+=, Ctrl+Shift+=, Ctrl+NumPlus" } else { "Ctrl+=, Ctrl+NumPlus" }),
    any(V, "zoom-out", if WIN { "Ctrl+-, Ctrl+Shift+-, Ctrl+NumMinus" } else { "Ctrl+-, Ctrl+NumMinus" }),
    any(V, "zoom-reset", if WIN { "Ctrl+0, Ctrl+Num0" } else { "Ctrl+0" }),
    any(V, "theme-system", ""),
    any(V, "theme-light", ""),
    any(V, "theme-dark", ""),
    any(V, "next-tab", "Ctrl+Tab, Ctrl+PageDown"),
    any(V, "prev-tab", "Ctrl+Shift+Tab, Ctrl+PageUp"),
    any(O, "format", "Shift+Alt+F"),
    only(any(O, "minify", ""), true),
    only(any(O, "check", ""), true),
    only(any(O, "json-format", ""), false),
    only(any(O, "json-minify", ""), false),
    only(any(O, "json-check", ""), false),
    only(any(O, "xml-format", ""), false),
    only(any(O, "xml-minify", ""), false),
    only(any(O, "xml-check", ""), false),
    only(any(O, "csv-check", ""), false),
    any(O, "csv-sort", ""),
    any(O, "csv-sort-desc", ""),
    any(O, "copy-json-path", ""),
    any(O, "eol-lf", ""),
    any(O, "eol-crlf", ""),
    text(O, "convert-base64-encode", ""),
    text(O, "convert-base64-decode", ""),
    text(O, "convert-url-encode", ""),
    text(O, "convert-url-decode", ""),
    text(O, "convert-json-escape", ""),
    text(O, "convert-json-unescape", ""),
    any(O, "convert-sha256", ""),
    any(H, "shortcuts", if WIN { "" } else { "Ctrl+?" }),
    any(H, "change-shortcuts", ""),
    any(H, "open-data-folder", ""),
    any(H, "make-default", ""),
    any(H, "stop-default", ""),
    only(any(H, "check-updates", ""), true),
    any(H, "report-problem", ""),
    any(H, "about", ""),
];

/// Edit → Multiple cursors' commands that add and take away cursors: their keys count only while the setting is on
/// (off, the keys do what they did before; the menu items say how to turn it on).
pub const CARET_COMMANDS: &[&str] = &[
    "add-cursor-above",
    "add-cursor-below",
    "add-next-match",
    "skip-match",
    "all-matches",
    "cursors-at-line-starts",
    "cursors-at-line-ends",
    "remove-last-cursor",
];

/// The commands in this app.
pub fn commands() -> impl Iterator<Item = &'static Command> {
    ALL.iter().filter(|c| c.only.is_none_or(|w| w == WIN))
}

pub fn command(id: &str) -> Option<&'static Command> {
    commands().find(|c| c.id == id)
}

/// The keys a command has unless the file says otherwise.
pub fn default_keys(c: &Command) -> Vec<Combo> {
    c.keys.split(',').filter_map(|k| Combo::parse(k.trim())).collect()
}

pub fn path() -> PathBuf {
    crate::settings::data_dir().join("shortcuts.txt")
}

/// Whether a tab's file is this one (no spell check there: command names and keys aren't words).
pub fn is_file(p: Option<&std::path::Path>) -> bool {
    p.is_some_and(|p| if cfg!(windows) { p.as_os_str().eq_ignore_ascii_case(path().as_os_str()) } else { p == path() })
}

/// What the file is written as the first time it's opened.
pub fn template() -> String {
    let mut s = String::from(
        "# Slate's keyboard shortcuts\n\
         #\n\
         # Every command is below with the keys it has. To give one other keys, take the # off the start of its\n\
         # line and write them after the =, commas between (or nothing, for none). Save, and Slate uses them at once.\n\
         # Those keys stop doing what they did before.\n\
         #\n\
         # Write keys as Ctrl, Shift and Alt with a letter, a digit, F1 to F24, Up, Down, Left, Right, Home, End,\n\
         # PageUp, PageDown, Insert, Delete, Backspace, Enter, Tab, Esc, Space, NumPlus, NumMinus, NumSlash, Num0 to\n\
         # Num9, or one of / \\ [ ] ; ' , . - = `  (Ctrl+Shift+D, Alt+[, F7). A shortcut needs Ctrl or Alt, or an F key.\n\
         # The keys for moving and typing (arrows, Home, Backspace, Ctrl+1 to 9…) stay as they are.\n",
    );
    let mut group = "";
    for c in commands() {
        if c.group != group {
            group = c.group;
            s.push_str(&format!("\n# {group}\n"));
        }
        let keys = default_keys(c).iter().map(Combo::text).collect::<Vec<_>>().join(", ");
        s.push_str(&format!("# {} = {}\n", c.id, keys));
    }
    s
}

/// The user's keys from the file's text: the commands given keys of their own, and what couldn't be used, by line.
#[derive(Default, Debug)]
pub struct User {
    pub keys: Vec<(&'static str, Vec<Combo>)>,
    pub problems: Vec<String>,
}

/// The keys after a line's `=`, split at the commas between them: a comma right after a `+` is the comma key
/// (`Ctrl+,`).
fn split_keys(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == ',' && !s[start..i].trim_end().ends_with('+') {
            out.push(s[start..i].trim());
            start = i + 1;
        }
    }
    out.push(s[start..].trim());
    out.retain(|k| !k.is_empty());
    out
}

pub fn parse(text: &str) -> User {
    let mut u = User::default();
    let mut owner: HashMap<Combo, (&'static str, usize)> = HashMap::new();
    // (a file saved with a byte order mark: it isn't part of the first command's name)
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((id, keys)) = line.split_once('=') else {
            u.problems.push(format!("Line {n}: no “=” between a command and its keys"));
            continue;
        };
        let id = id.trim().to_ascii_lowercase();
        let Some(c) = command(&id) else {
            u.problems.push(format!("Line {n}: Slate has no command “{id}”"));
            continue;
        };
        let mut combos = Vec::new();
        let mut wrong = Vec::new();
        for k in split_keys(keys) {
            match Combo::parse(k) {
                None => wrong.push(format!("Line {n}: “{k}” isn't a key Slate knows")),
                Some(combo) if !combo.usable() => {
                    wrong.push(format!("Line {n}: {k} needs Ctrl or Alt (or an F key), or it would stop typing"))
                }
                Some(combo) => match owner.get(&combo) {
                    Some(&(other, line)) if other != c.id => {
                        wrong.push(format!("Line {n}: {} is already {other}'s (line {line})", combo.text()))
                    }
                    _ if !combos.contains(&combo) => combos.push(combo),
                    _ => {}
                },
            }
        }
        // (a line with a mistake changes nothing: its command keeps the keys it had, and the message says so)
        if let Some(last) = wrong.last_mut() {
            last.push_str(&format!(", so {} keeps the keys it had", c.id));
            u.problems.append(&mut wrong);
            continue;
        }
        for combo in &combos {
            owner.insert(*combo, (c.id, n));
        }
        // (a command named twice: the last line counts)
        u.keys.retain(|(i, _)| *i != c.id);
        u.keys.push((c.id, combos));
    }
    u
}

/// The file read (no file: no keys of the user's). Up to 1 MB.
pub fn read() -> User {
    match std::fs::read(path()) {
        Ok(b) if b.len() <= 1 << 20 => parse(&String::from_utf8_lossy(&b)),
        Ok(_) => User { keys: Vec::new(), problems: vec!["shortcuts.txt is over 1 MB: Slate didn't read it".into()] },
        Err(_) => User::default(),
    }
}

/// The user's keys laid over the apps' own.
#[derive(Default)]
pub struct Keymap {
    by_combo: HashMap<Combo, &'static str>,
    by_id: HashMap<&'static str, Vec<Combo>>,
}

impl Keymap {
    pub fn new(user: &User) -> Keymap {
        let mut k = Keymap::default();
        for (id, combos) in &user.keys {
            for c in combos {
                k.by_combo.insert(*c, id);
            }
            k.by_id.insert(id, combos.clone());
        }
        k
    }

    /// The command the user gave these keys to.
    pub fn command_for(&self, c: &Combo) -> Option<&'static Command> {
        self.by_combo.get(c).and_then(|id| command(id))
    }

    /// The user's keys for a command (Some(empty): none), or None: it has its own.
    pub fn keys_of(&self, id: &str) -> Option<&[Combo]> {
        self.by_id.get(id).map(Vec::as_slice)
    }

    /// Whether the user gave these keys to some command (so they no longer do what they did).
    pub fn taken(&self, c: &Combo) -> bool {
        self.by_combo.contains_key(c)
    }

    /// Whether an app's own match of these keys to `id` still counts.
    pub fn keeps(&self, id: &str, c: &Combo) -> bool {
        !self.by_id.contains_key(id) && !self.taken(c)
    }

    /// For the menus: the user's keys for a command, if it has some of the user's ("" for none).
    pub fn menu_text(&self, id: &str) -> Option<String> {
        self.keys_of(id).map(|v| v.first().map(Combo::text).unwrap_or_default())
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// For Help → Keyboard shortcuts: the commands given other keys, a line each.
    pub fn summary(&self) -> String {
        commands()
            .filter_map(|c| self.keys_of(c.id).map(|v| (c.id, v)))
            .map(|(id, v)| {
                let keys = v.iter().map(Combo::text).collect::<Vec<_>>().join(", ");
                format!("  {:<30}{}\n", if keys.is_empty() { "(none)".to_string() } else { keys }, id)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_read_and_written_back() {
        for s in ["Ctrl+Shift+D", "Shift+Alt+F", "F3", "Ctrl+/", "Alt+[", "Ctrl+PageDown", "Ctrl+NumPlus", "Ctrl+Num0"] {
            assert_eq!(Combo::parse(s).unwrap().text(), s);
        }
        assert_eq!(Combo::parse("control+shift+d"), Combo::parse("Ctrl+Shift+D"));
        assert_eq!(Combo::parse("Ctrl+Plus").unwrap().text(), "Ctrl+=");
        assert_eq!(Combo::parse("Alt+Return").unwrap().key, Key::Enter);
        assert_eq!(Combo::parse("Ctrl+Shift+U").unwrap().gtk(), "<Control><Shift>u");
        assert_eq!(Combo::parse("Alt+]").unwrap().gtk(), "<Alt>bracketright");
        for bad in ["Ctrl+", "Ctrl+Foo", "Hyper+D", "F25", "Ctrl+Shift", ""] {
            assert!(Combo::parse(bad).is_none(), "{bad}");
        }
        assert!(!Combo::parse("D").unwrap().usable());
        assert!(!Combo::parse("Shift+D").unwrap().usable());
        assert!(Combo::parse("F7").unwrap().usable());
    }

    #[test]
    fn every_default_is_a_key_and_no_two_commands_share_one() {
        let mut seen = HashMap::new();
        for c in commands() {
            for k in c.keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                let combo = Combo::parse(k).unwrap_or_else(|| panic!("{}: {k}", c.id));
                if let Some(other) = seen.insert(combo, c.id) {
                    panic!("{k}: {other} and {}", c.id);
                }
            }
        }
        let ids: Vec<_> = commands().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len());
    }

    #[test]
    fn the_file_gives_commands_other_keys() {
        // the template changes nothing
        let u = parse(&template());
        assert!(u.keys.is_empty() && u.problems.is_empty(), "{:?}", u.problems);
        let u = parse(
            "duplicate-line = Ctrl+Shift+D  # mine\n\
             goto-line = Ctrl+L, Alt+G\n\
             upper =\n\
             DELETE-LINE=ctrl+shift+k\n\
             nothing = Ctrl+1\n\
             find = Ctrl+Foo, Q\n\
             replace = Ctrl+L\n\
             just words\n",
        );
        let k = Keymap::new(&u);
        let d = Combo::parse("Ctrl+Shift+D").unwrap();
        assert_eq!(k.command_for(&d).map(|c| c.id), Some("duplicate-line"));
        assert_eq!(k.keys_of("upper"), Some(&[][..]));
        assert_eq!(k.menu_text("goto-line").as_deref(), Some("Ctrl+L"));
        assert_eq!(k.menu_text("upper").as_deref(), Some(""));
        assert_eq!(k.menu_text("find-next"), None);
        // the default keys of a command given others stop counting, as do keys given to another one
        assert!(!k.keeps("duplicate-line", &Combo::parse("Ctrl+D").unwrap()));
        assert!(!k.keeps("goto-line", &Combo::parse("Ctrl+G").unwrap()));
        assert!(k.keeps("find-next", &Combo::parse("F3").unwrap()));
        assert!(!k.keeps("structure-panel", &Combo::parse("Ctrl+Shift+D").unwrap()));
        // (a line with a mistake changes nothing: its command keeps the keys it had, and the message says so)
        assert_eq!(k.keys_of("find"), None);
        assert_eq!(k.keys_of("replace"), None);
        let problems = u.problems.join("\n");
        for p in [
            "Line 5: Slate has no command “nothing”",
            "“Ctrl+Foo” isn't a key",
            "Line 6: Q needs Ctrl or Alt (or an F key), or it would stop typing, so find keeps the keys it had",
            "Line 7: Ctrl+L is already goto-line's (line 2), so replace keeps the keys it had",
            "Line 8: no “=”",
        ] {
            assert!(problems.contains(p), "{p} in {problems}");
        }
        assert!(k.summary().contains("Ctrl+Shift+D") && k.summary().contains("(none)"));
        // the comma key, after a `+`; a byte order mark before the first line
        let u = parse("\u{FEFF}duplicate-line = Ctrl+,, Alt+X\nupper = Ctrl+Shift+,\n");
        assert!(u.problems.is_empty(), "{:?}", u.problems);
        let k = Keymap::new(&u);
        assert_eq!(k.menu_text("duplicate-line").as_deref(), Some("Ctrl+,"));
        assert_eq!(k.keys_of("duplicate-line").map(<[Combo]>::len), Some(2));
        assert_eq!(k.menu_text("upper").as_deref(), Some("Ctrl+Shift+,"));
        // (Windows: ? is Shift and the / key, which is what a key press there gives)
        #[cfg(windows)]
        assert_eq!(Combo::parse("Ctrl+?"), Combo::parse("Ctrl+Shift+/"));
    }
}
