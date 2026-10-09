//! User settings, stored as JSON in settings.json in the data folder (`data_dir`: %LOCALAPPDATA%\Slate on Windows,
//! ~/.local/share/slate on Linux, or the `data` folder next to a portable Slate).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

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
    pub theme: ThemeMode,
    pub tab_size: u32,
    pub use_spaces: bool,
    pub json_indent: u32,
    pub restore_session: bool,
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
            theme: ThemeMode::System,
            tab_size: 4,
            use_spaces: true,
            json_indent: 2,
            restore_session: true,
            path_bar: true,
            structure_panel: false,
            structure_width: 340.0,
            recent: Vec::new(),
            window: None,
            check_updates: true,
            last_update_check: 0,
            failed_update: String::new(),
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

pub fn data_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
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
        return std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Slate");
        // (XDG: $XDG_DATA_HOME, else ~/.local/share)
        #[cfg(not(windows))]
        return std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
            .unwrap_or_else(std::env::temp_dir)
            .join("slate");
    })
    .clone()
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
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let Ok(json) = serde_json::to_vec_pretty(self) else { return };
        let tmp = dir.join("settings.json.tmp");
        // On the disk before it takes the name (after a power cut a renamed file can otherwise come back empty).
        let written = std::fs::File::create(&tmp).and_then(|mut f| f.write_all(&json).and_then(|_| f.sync_all()));
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, Self::path());
        }
    }

    pub fn add_recent(&mut self, p: &std::path::Path) {
        self.recent.retain(|r| r != p);
        self.recent.insert(0, p.to_path_buf());
        self.recent.truncate(15);
    }

    pub fn indent_unit(&self) -> Vec<u8> {
        if self.use_spaces { vec![b' '; self.tab_size as usize] } else { b"\t".to_vec() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
