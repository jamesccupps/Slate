//! "Open files with Slate…": installs Slate for the current user (no admin needed), adds it to the Start menu, to
//! the "Open with" list of text-like file types and to the right-click menu ("Edit with Slate"), then opens
//! Windows' Default apps page for Slate. Windows doesn't let apps make themselves the default; the user picks.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_NONE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegSetValueExW,
};
use windows::Win32::UI::Shell::{IShellLinkW, SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, Interface, PCWSTR, w};

use super::app::Cell;
use super::win;

const PROGID: &str = "Slate.TextFile";
const EXTENSIONS: &[&str] = &[
    // text, data and config
    ".txt", ".log", ".json", ".jsonl", ".ndjson", ".geojson", ".md", ".markdown", ".csv", ".tsv", ".xml", ".ini",
    ".cfg", ".conf", ".config", ".yaml", ".yml", ".toml", ".properties", ".env", ".nfo", ".diz", ".out", ".srt",
    ".sql", ".reg", ".inf", ".editorconfig", ".gitignore", ".gitattributes", ".diff", ".patch",
    // scripts
    ".ps1", ".psm1", ".psd1", ".bat", ".cmd", ".sh", ".bash", ".py", ".pyw", ".rb", ".lua", ".pl", ".vbs",
    // web
    ".html", ".htm", ".xhtml", ".css", ".scss", ".less", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".vue",
    ".svg", ".php",
    // code and project files
    ".c", ".h", ".cpp", ".hpp", ".cc", ".cxx", ".ino", ".cs", ".csproj", ".sln", ".props", ".targets", ".xaml",
    ".resx", ".java", ".kt", ".kts", ".gradle", ".swift", ".go", ".rs", ".dart",
];

fn set(path: &str, name: Option<&str>, value: &str) -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .is_err()
        {
            return false;
        }
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let name_h = name.map(HSTRING::from);
        let pname = name_h.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let ok = RegSetValueExW(key, pname, 0, REG_SZ, Some(bytes)).is_ok();
        let _ = RegCloseKey(key);
        ok
    }
}

fn set_empty(path: &str, name: &str) -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), 0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .is_err()
        {
            return false;
        }
        let ok = RegSetValueExW(key, &HSTRING::from(name), 0, REG_NONE, None).is_ok();
        let _ = RegCloseKey(key);
        ok
    }
}

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("Slate")
}

/// Copies the running Slate.exe to the per-user install folder (a running older copy is renamed aside first).
fn install_exe() -> std::io::Result<PathBuf> {
    let me = std::env::current_exe()?;
    if super::settings::portable() {
        return Ok(me);
    }
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join("Slate.exe");
    if dest.exists() && crate::core::io::same_file(&me, &dest) {
        return Ok(dest);
    }
    if dest.exists() {
        // Windows lets a running exe be renamed but not overwritten.
        let old = dir.join(format!("Slate.old-{}.exe", std::process::id()));
        std::fs::rename(&dest, &old)?;
    }
    std::fs::copy(&me, &dest)?;
    Ok(dest)
}

/// Removes leftovers of earlier updates (old copies that were still running then).
pub fn clean_old_copies() {
    // In the install folder and next to the running exe (a portable copy, or one an update replaced).
    let dirs = [Some(install_dir()), super::settings::exe_dir()];
    for dir in dirs.iter().flatten() {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if (n.starts_with("Slate.old-") || n.starts_with("Slate.update-")) && n.ends_with(".exe") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn start_menu_shortcut(exe: &Path) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let lnk = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Slate.lnk");
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return };
        let _ = link.SetPath(&HSTRING::from(exe.as_os_str()));
        let _ = link.SetDescription(w!("Slate text editor"));
        if let Ok(pf) = link.cast::<IPersistFile>() {
            let _ = pf.Save(&HSTRING::from(lnk.as_os_str()), true);
        }
    }
}

fn register(exe: &Path) -> bool {
    let e = exe.display().to_string();
    let open = format!("\"{e}\" \"%1\"");
    let icon = format!("\"{e}\",0");
    let mut ok = true;
    ok &= set(&format!(r"Software\Classes\{PROGID}"), None, "Text document");
    ok &= set(&format!(r"Software\Classes\{PROGID}\DefaultIcon"), None, &icon);
    ok &= set(&format!(r"Software\Classes\{PROGID}\shell\open\command"), None, &open);
    ok &= set(r"Software\Classes\Applications\Slate.exe", Some("FriendlyAppName"), "Slate");
    ok &= set(r"Software\Classes\Applications\Slate.exe\shell\open\command", None, &open);
    for ext in EXTENSIONS {
        ok &= set_empty(&format!(r"Software\Classes\{ext}\OpenWithProgids"), PROGID);
        ok &= set(r"Software\Classes\Applications\Slate.exe\SupportedTypes", Some(ext), "");
        ok &= set(r"Software\Slate\Capabilities\FileAssociations", Some(ext), PROGID);
    }
    // Right-click any file: "Edit with Slate".
    ok &= set(r"Software\Classes\*\shell\Slate", None, "Edit with Slate");
    ok &= set(r"Software\Classes\*\shell\Slate", Some("Icon"), &icon);
    ok &= set(r"Software\Classes\*\shell\Slate\command", None, &open);
    // Default apps (Settings) listing.
    ok &= set(r"Software\Slate\Capabilities", Some("ApplicationName"), "Slate");
    ok &= set(r"Software\Slate\Capabilities", Some("ApplicationDescription"), "A fast, simple text editor that opens files of any size.");
    ok &= set(r"Software\RegisteredApplications", Some("Slate"), r"Software\Slate\Capabilities");
    // Apps & features entry so it can be found later.
    let un = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate";
    ok &= set(un, Some("DisplayName"), "Slate");
    ok &= set(un, Some("DisplayIcon"), &icon);
    ok &= set(un, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    ok &= set(un, Some("Publisher"), "Slate");
    let loc = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    ok &= set(un, Some("InstallLocation"), &loc);
    ok &= set(un, Some("UninstallString"), &format!("\"{e}\" --uninstall"));
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    ok
}

pub fn make_default(cell: &Cell) {
    let hwnd = cell.borrow().hwnd;
    let place = if super::settings::portable() {
        format!("Slate stays where it is ({}) and gets", super::settings::exe_dir().unwrap_or_default().display())
    } else {
        "Slate gets installed for your account (no admin rights needed),".to_string()
    };
    let detail = format!(
        "{place} added to the Start menu, to \"Open with\" for text, data, config, script, web and code files, and as \
         \"Edit with Slate\" when you right-click a file.\n\nThen Windows' Default apps page opens, where you can choose Slate \
         for the file types you want (Windows only lets you do that step yourself)."
    );
    if win::ask(hwnd, "Slate", "Open your text files with Slate?", &detail, &["Set up", "Cancel"]) != Some(0) {
        return;
    }
    let exe = match install_exe() {
        Ok(p) => p,
        Err(e) => {
            win::info(hwnd, "Slate", &format!("Couldn't install Slate: {e}"));
            return;
        }
    };
    start_menu_shortcut(&exe);
    if !register(&exe) {
        win::info(hwnd, "Slate", "Some settings couldn't be saved to the registry.");
    }
    unsafe {
        ShellExecuteW(hwnd, w!("open"), w!("ms-settings:defaultapps?registeredAppUser=Slate"), None, None, SW_SHOWNORMAL);
    }
    cell.borrow_mut().flash("Slate is set up. Pick it in Default apps for the file types you want.", false);
}

/// `Slate.exe --uninstall`: removes what `make_default` added (the program folder is removed at the next sign-in).
pub fn uninstall() {
    use windows::Win32::System::Registry::RegDeleteTreeW;
    unsafe {
        for k in [
            format!(r"Software\Classes\{PROGID}"),
            r"Software\Classes\Applications\Slate.exe".to_string(),
            r"Software\Classes\*\shell\Slate".to_string(),
            r"Software\Slate".to_string(),
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate".to_string(),
        ] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
        }
        for ext in EXTENSIONS {
            let path = HSTRING::from(format!(r"Software\Classes\{ext}\OpenWithProgids"));
            let mut key = HKEY::default();
            if windows::Win32::System::Registry::RegOpenKeyExW(HKEY_CURRENT_USER, &path, 0, KEY_WRITE, &mut key).is_ok() {
                let _ = windows::Win32::System::Registry::RegDeleteValueW(key, &HSTRING::from(PROGID));
                let _ = RegCloseKey(key);
            }
        }
        let mut key = HKEY::default();
        if windows::Win32::System::Registry::RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Software\RegisteredApplications"), 0, KEY_WRITE, &mut key)
            .is_ok()
        {
            let _ = windows::Win32::System::Registry::RegDeleteValueW(key, w!("Slate"));
            let _ = RegCloseKey(key);
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let _ = std::fs::remove_file(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Slate.lnk"));
        }
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
        win::info(windows::Win32::Foundation::HWND::default(), "Slate", "Slate was removed from your account's settings. You can now delete Slate's folder.");
    }
}
