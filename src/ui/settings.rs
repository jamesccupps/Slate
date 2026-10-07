//! User settings, stored as JSON in %LOCALAPPDATA%\Slate\settings.json.

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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            font: "Cascadia Mono".into(),
            font_size: 11.0,
            zoom: 1.0,
            wrap: true,
            line_numbers: true,
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
        }
    }
}

/// Set in test mode: never write settings or the session.
pub static NO_PERSIST: AtomicBool = AtomicBool::new(false);

pub fn persist() -> bool {
    !NO_PERSIST.load(Ordering::Relaxed)
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
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join("Slate")
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
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .map(|mut s| {
                s.zoom = s.zoom.clamp(0.5, 5.0);
                s.font_size = s.font_size.clamp(6.0, 72.0);
                s.tab_size = s.tab_size.clamp(1, 16);
                s.json_indent = s.json_indent.clamp(1, 8);
                s
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if !persist() {
            return;
        }
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = dir.join("settings.json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, Self::path());
            }
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
