//! User settings, stored as JSON in settings.json in the data folder (`data_dir`: %LOCALAPPDATA%\Slate Text Editor
//! on Windows, ~/.local/share/slate on Linux, or the `data` folder next to a portable Slate).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

/// The zoom levels Zoom in and Zoom out step through (the settings keep one between the first and the last).
pub const ZOOM_STEPS: [f32; 15] = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Where the caret was in a file when its tab closed (`Settings::places`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilePlace {
    pub path: PathBuf,
    pub caret: u64,
    pub top: u64,
}

/// Files whose place is kept (`Settings::places`).
pub const PLACES_MAX: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font: String,
    /// Points.
    pub font_size: f32,
    pub zoom: f32,
    pub wrap: bool,
    pub line_numbers: bool,
    /// Dots for spaces, arrows for tabs, marks for line breaks.
    pub show_whitespace: bool,
    /// View → Hide the menu bar (issue #10): it shows while Alt (or F10) gives it the keyboard, or a menu is open.
    pub hide_menu_bar: bool,
    /// Words spelled wrong underlined (`crate::spell`).
    pub spell_check: bool,
    /// Windows: the tabs in the title bar, as in Windows 11's Notepad (Windows' own title bar in high contrast).
    pub tabs_in_title_bar: bool,
    /// View → Close Slate with the last tab: closing the only tab (Ctrl+W, its ×) closes Slate too; off, an empty tab
    /// stays, as in Notepad.
    pub close_with_last_tab: bool,
    /// View → Find bar → Close when you click in the text: the keyboard leaving the find bar (or Go to's) for the
    /// text closes it (issue #6). Off: it stays open until Esc.
    pub find_close_on_leave: bool,
    /// View → Find bar → One for each tab: each tab keeps its find bar open or closed, a new one's closed; the
    /// search itself is shared. Off: one bar for the window, open whichever tab shows.
    pub find_per_tab: bool,
    /// View → Find bar → At the bottom (issue #9): the find bar and a tab's notice sit above the status bar, so opening
    /// them doesn't move the text.
    pub bars_at_bottom: bool,
    /// Edit → Multiple cursors → Use multiple cursors: Ctrl+click and the Multiple cursors commands make more than one
    /// (issue #5). Off: Ctrl+click does what it always did and the commands only say how to turn it on.
    pub multi_cursor: bool,
    pub theme: ThemeMode,
    pub tab_size: u32,
    pub use_spaces: bool,
    pub json_indent: u32,
    pub restore_session: bool,
    /// File → Reopen files where you left them: a file opened again starts where its caret and scroll were when its
    /// tab closed (`places`, issue #11).
    pub reopen_where_left: bool,
    /// Those places, newest first (`PLACES_MAX`).
    pub places: Vec<FilePlace>,
    /// JSON: the path bar above the text, and the structure panel (with its width in DIPs).
    pub path_bar: bool,
    pub structure_panel: bool,
    pub structure_width: f32,
    pub recent: Vec<PathBuf>,
    pub window: Option<Placement>,
    /// Look for a new version on GitHub (at most once a day), and when that last happened (Unix seconds).
    pub check_updates: bool,
    pub last_update_check: u64,
    /// A version whose update didn't start on this PC (and was undone): not offered again by the daily check.
    pub failed_update: String,
    /// The last searches and replacements, newest first (Up and Down in the find bar's boxes).
    pub find_history: Vec<String>,
    pub replace_history: Vec<String>,
    /// Printing: the margins from the paper's edges in hundredths of a millimetre (left, top, right, bottom), the
    /// text in its colors, line numbers, and the file's name at the top of each page with its number at the bottom.
    pub print_margins: [u32; 4],
    pub print_colors: bool,
    pub print_line_numbers: bool,
    pub print_header: bool,
    /// The user's own file endings and their languages (`{"mmd": "Markdown"}`, issue #13; Format → Language →
    /// Always open .x files as …): `highlight::set_user_langs`.
    pub languages: std::collections::BTreeMap<String, String>,
    /// Settings this version doesn't know (written by a newer one), kept for it.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            font: if cfg!(windows) { "Cascadia Mono" } else { "Monospace" }.into(),
            font_size: 11.0,
            zoom: 1.0,
            wrap: true,
            line_numbers: true,
            show_whitespace: false,
            hide_menu_bar: false,
            spell_check: true,
            tabs_in_title_bar: true,
            close_with_last_tab: false,
            find_close_on_leave: false,
            find_per_tab: false,
            bars_at_bottom: false,
            multi_cursor: false,
            theme: ThemeMode::System,
            tab_size: 4,
            use_spaces: true,
            json_indent: 2,
            restore_session: true,
            reopen_where_left: true,
            places: Vec::new(),
            path_bar: true,
            structure_panel: false,
            structure_width: 340.0,
            recent: Vec::new(),
            window: None,
            check_updates: true,
            last_update_check: 0,
            failed_update: String::new(),
            find_history: Vec::new(),
            replace_history: Vec::new(),
            print_margins: [1905; 4],
            print_colors: true,
            print_line_numbers: true,
            print_header: true,
            languages: std::collections::BTreeMap::new(),
            other: serde_json::Map::new(),
        }
    }
}

/// Set in test mode: never write settings or the session.
pub static NO_PERSIST: AtomicBool = AtomicBool::new(false);

pub fn persist() -> bool {
    !NO_PERSIST.load(Ordering::Relaxed) && !guest()
}

/// Set when this Slate started while another one was running but didn't answer (busy or hung), so it couldn't take
/// the files: this one opens them in a window of its own that leaves that Slate's settings and session alone (it
/// keeps nothing for next time, so closing asks about unsaved changes).
pub static GUEST: AtomicBool = AtomicBool::new(false);

pub fn guest() -> bool {
    GUEST.load(Ordering::Relaxed)
}

/// The folder Slate.exe runs from.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// Portable mode: a `Slate.portable` file next to Slate.exe. Settings, the session and big temporary files then
/// live in a `data` folder beside the exe (e.g. all on a fast drive) instead of AppData and %TEMP%.
pub fn portable() -> bool {
    exe_dir().is_some_and(|d| d.join("Slate.portable").exists())
}

/// The data folder, once worked out (`move_data_dir` changes it).
static DATA_DIR: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

pub fn data_dir() -> PathBuf {
    if let Some(d) = DATA_DIR.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return d.clone();
    }
    let d = find_data_dir();
    *DATA_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(d.clone());
    d
}

// (each platform's part returns: Windows' `return` reads as the other's, though it's the last expression there)
#[cfg_attr(windows, allow(clippy::needless_return))]
fn find_data_dir() -> PathBuf {
    // For tests: a separate data folder.
    if let Some(d) = std::env::var_os("SLATE_DATA_DIR") {
        return PathBuf::from(d);
    }
    if portable() {
        if let Some(d) = exe_dir() {
            return d.join("data");
        }
    }
    #[cfg(windows)]
    {
        // (0.15.0 and older kept it in %LOCALAPPDATA%\Slate: used there until `move_data_dir` moves it)
        let (new, old) = windows_data_dirs();
        return if !new.exists() && is_slates_folder(&old) { old } else { new };
    }
    // (XDG: $XDG_DATA_HOME, else ~/.local/share)
    #[cfg(not(windows))]
    return match std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
    {
        Some(base) => base.join("slate"),
        // No home (`env -i`, a service): a folder of this user's own in the temp folder. A fixed `/tmp/slate` could be
        // someone else's, made first, who could then change the session's files under Slate; one that isn't this
        // user's alone keeps nothing (a guest window).
        None => {
            let d = std::env::temp_dir().join(format!("slate-{}", unsafe { libc::getuid() }));
            if !private_dir(&d) {
                GUEST.store(true, Ordering::Relaxed);
            }
            d
        }
    };
}

/// Makes `d` (mode 0700) if it isn't there, and says whether it's a folder of this user's that only they can use (not
/// a link to one).
#[cfg(not(windows))]
fn private_dir(d: &std::path::Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let _ = std::fs::DirBuilder::new().mode(0o700).create(d);
    std::fs::symlink_metadata(d)
        .is_ok_and(|m| m.is_dir() && m.uid() == unsafe { libc::getuid() } && m.mode() & 0o077 == 0)
}

/// Where Windows keeps the data folder: `%LOCALAPPDATA%\Slate Text Editor` (so another program called Slate can't
/// share it), and `%LOCALAPPDATA%\Slate`, where 0.15.0 and older kept it.
#[cfg(windows)]
fn windows_data_dirs() -> (PathBuf, PathBuf) {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    (base.join("Slate Text Editor"), base.join("Slate"))
}

/// Whether `d` is a data folder Slate wrote: its settings (with a setting only Slate has) or its session.
#[cfg(windows)]
fn is_slates_folder(d: &std::path::Path) -> bool {
    let settings = std::fs::read(d.join("settings.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    settings.is_some_and(|v| v.get("restore_session").is_some() && v.get("tabs_in_title_bar").or(v.get("json_indent")).is_some())
        || d.join("session").join("session.json").is_file()
        || d.join("session-admin").join("session.json").is_file()
}

/// Moves the data folder of 0.15.0 and older (`%LOCALAPPDATA%\Slate`) to where it's kept now, once. Only at a start
/// when no other Slate runs (it would go on using the old folder). One that can't be moved now (a file in it is open)
/// stays in use where it is, and is moved at a later start. Returns whether it moved.
#[cfg(windows)]
pub fn move_data_dir() -> bool {
    if std::env::var_os("SLATE_DATA_DIR").is_some() || portable() {
        return false;
    }
    let (new, old) = windows_data_dirs();
    let moved = move_data_dir_in(&new, &old);
    if moved {
        *DATA_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(new);
    }
    moved
}

#[cfg(windows)]
fn move_data_dir_in(new: &std::path::Path, old: &std::path::Path) -> bool {
    if new.exists() || !is_slates_folder(old) {
        return false;
    }
    std::fs::rename(old, new).is_ok()
}

/// How big crash.log gets before it starts over (the one before kept as crash.log.old).
const CRASH_LOG_MAX: u64 = 1 << 20;

/// Makes the data folder if it isn't there, and returns it. On Linux it's its owner's only (0700): it holds unsaved
/// text, text piped in, the find history (one made by 0.16.0 or older was 0755, readable by other users: made the
/// owner's).
pub fn make_data_dir() -> PathBuf {
    let dir = data_dir();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        if std::fs::metadata(&dir).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Adds `line` to crash.log in the data folder. A panic that comes back at every repaint would otherwise make it grow
/// for as long as Slate runs: past `CRASH_LOG_MAX` it starts over, keeping the last one beside it.
pub fn append_crash_log(line: &str) {
    let dir = make_data_dir();
    append_capped(&dir.join("crash.log"), line, CRASH_LOG_MAX);
}

/// Appends `line` to `log`, first moving a log of `max` bytes or more to `<log>.old` (replacing the one there).
fn append_capped(log: &std::path::Path, line: &str, max: u64) {
    use std::io::Write;
    if std::fs::metadata(log).is_ok_and(|m| m.len() >= max) {
        let mut old = log.as_os_str().to_owned();
        old.push(".old");
        let _ = std::fs::rename(log, old);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Where big temporary files go (results of formatting or converting huge files).
pub fn temp_dir() -> PathBuf {
    if portable() { data_dir().join("temp") } else { std::env::temp_dir() }
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        let mut s = std::fs::read(Self::path()).map(|b| Self::from_json(&b)).unwrap_or_default();
        s.zoom = s.zoom.clamp(0.5, 5.0);
        s.font_size = s.font_size.clamp(6.0, 72.0);
        s.tab_size = s.tab_size.clamp(1, 16);
        s.json_indent = s.json_indent.clamp(1, 8);
        s
    }

    /// One setting that can't be used (damaged, or a value from a newer version) keeps its default instead of
    /// resetting all of them.
    pub fn from_json(bytes: &[u8]) -> Settings {
        use serde_json::Value;
        let Ok(Value::Object(file)) = serde_json::from_slice::<Value>(bytes) else { return Settings::default() };
        let Ok(Value::Object(mut merged)) = serde_json::to_value(Settings::default()) else { return Settings::default() };
        for (k, v) in file {
            let mut trial = merged.clone();
            trial.insert(k.clone(), v.clone());
            if serde_json::from_value::<Settings>(Value::Object(trial)).is_ok() {
                merged.insert(k, v);
            }
        }
        serde_json::from_value(Value::Object(merged)).unwrap_or_default()
    }

    pub fn save(&self) {
        use std::io::Write;
        if !persist() {
            return;
        }
        let dir = make_data_dir();
        let Ok(json) = serde_json::to_vec_pretty(self) else { return };
        // (named for this process: an administrator's Slate and another one saving at once each write their own)
        let tmp = dir.join(format!("settings.json.{}.tmp", std::process::id()));
        // On the disk before it takes the name (after a power cut a renamed file can otherwise come back empty).
        let written = std::fs::File::create(&tmp).and_then(|mut f| f.write_all(&json).and_then(|_| f.sync_all()));
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, Self::path());
        }
    }

    pub fn add_recent(&mut self, p: &std::path::Path) {
        // (settings.json can only hold names that are text: one that isn't would keep it from being written at all)
        if p.to_str().is_none() {
            return;
        }
        self.recent.retain(|r| r != p);
        self.recent.insert(0, p.to_path_buf());
        self.recent.truncate(15);
    }

    /// The language the user set for files ending in `ext` (`highlight::ending_of`'s form), if any.
    pub fn lang_for_ending(&self, ext: &str) -> Option<crate::highlight::Lang> {
        let key = |k: &str| k.trim().trim_start_matches('.').to_ascii_lowercase();
        self.languages.iter().find(|(k, _)| key(k) == ext).and_then(|(_, n)| crate::highlight::Lang::by_name(n))
    }

    /// Format → Language → Always open .x files as … for a file ending in `ext` shown in `lang`: off if it's on,
    /// else on (issue #13). Returns what to say, and whether it's on now.
    pub fn toggle_lang_for_ending(&mut self, ext: &str, lang: crate::highlight::Lang) -> (String, bool) {
        let on = self.lang_for_ending(ext) != Some(lang);
        let key = |k: &str| k.trim().trim_start_matches('.').to_ascii_lowercase();
        self.languages.retain(|k, _| key(k) != ext);
        if on {
            self.languages.insert(ext.to_string(), lang.label().to_string());
        }
        crate::highlight::set_user_langs(&self.languages);
        let msg = if on {
            format!("Files ending in .{ext} open as {} from now on", lang.label())
        } else {
            format!("Files ending in .{ext} get their language the usual way again")
        };
        (msg, on)
    }

    /// Notes where the caret and the top of the view were in `path` as its tab closes (issue #11): one place per file,
    /// newest first; none for the start of the file.
    pub fn remember_place(&mut self, path: &std::path::Path, caret: u64, top: u64) {
        if !self.reopen_where_left || path.to_str().is_none() {
            return;
        }
        self.places.retain(|p| p.path != path);
        if caret > 0 || top > 0 {
            self.places.insert(0, FilePlace { path: path.to_path_buf(), caret, top });
            self.places.truncate(PLACES_MAX);
        }
    }

    /// Where `path` was left (caret, top), if it's to be opened there.
    pub fn place_of(&self, path: &std::path::Path) -> Option<(u64, u64)> {
        self.places.iter().find(|p| p.path == path).filter(|_| self.reopen_where_left).map(|p| (p.caret, p.top))
    }

    pub fn indent_unit(&self) -> Vec<u8> {
        if self.use_spaces { vec![b' '; self.tab_size as usize] } else { b"\t".to_vec() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn the_fallback_data_folder_is_used_only_when_its_this_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("slate-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mine = base.join("mine");
        assert!(private_dir(&mine));
        assert!(private_dir(&mine), "made before: still fine");
        // open to others (made first by someone, or chmod'ed): not used
        let open = base.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!private_dir(&open));
        // a link to a private folder: not used either (it could point anywhere)
        let link = base.join("link");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        assert!(!private_dir(&link));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn the_old_data_folder_moves_only_when_its_slates() {
        let base = std::env::temp_dir().join(format!("slate-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (new, old) = (base.join("Slate Text Editor"), base.join("Slate"));
        // another program's folder of that name stays where it is
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), br#"{"theme": "dark"}"#).unwrap();
        assert!(!move_data_dir_in(&new, &old));
        assert!(old.exists() && !new.exists());
        // Slate's (its settings, a session) moves, all of it
        std::fs::write(old.join("settings.json"), br#"{"restore_session": true, "json_indent": 2}"#).unwrap();
        std::fs::create_dir_all(old.join("session")).unwrap();
        std::fs::write(old.join("session").join("session.json"), b"{}").unwrap();
        assert!(move_data_dir_in(&new, &old));
        assert!(!old.exists() && new.join("session").join("session.json").exists());
        // once only: a folder made there again later isn't moved over it
        std::fs::create_dir_all(old.join("session")).unwrap();
        std::fs::write(old.join("session").join("session.json"), b"{}").unwrap();
        assert!(!move_data_dir_in(&new, &old));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_crash_log_starts_over_when_full() {
        let dir = std::env::temp_dir().join(format!("slate-crashlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("crash.log");
        for i in 0..10 {
            append_capped(&log, &format!("line {i}\n"), 21);
        }
        // 3 lines (7 bytes each) fill it: each fourth starts it over, the full one kept
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "line 9\n");
        assert_eq!(std::fs::read_to_string(dir.join("crash.log.old")).unwrap(), "line 6\nline 7\nline 8\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_or_newer_value_keeps_the_other_settings() {
        let s = Settings::from_json(br#"{"font_size": 14.0, "theme": "Sepia", "wrap": false, "new_thing": {"a": 1}}"#);
        assert_eq!(s.font_size, 14.0);
        assert_eq!(s.theme, ThemeMode::System);
        assert!(!s.wrap);
        // kept for the version that knows it
        let back = serde_json::to_value(&s).unwrap();
        assert_eq!(back["new_thing"]["a"], 1);
        // damaged: defaults
        assert_eq!(Settings::from_json(b"{\"font_si").font_size, 11.0);
    }
}
