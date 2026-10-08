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
    ".vtt", ".ics", ".vcf", ".sql", ".reg", ".inf", ".editorconfig", ".gitignore", ".gitattributes", ".npmrc",
    ".htaccess", ".diff", ".patch", ".j2", ".jinja",
    // scripts
    ".ps1", ".psm1", ".psd1", ".bat", ".cmd", ".sh", ".bash", ".py", ".pyw", ".rb", ".lua", ".pl", ".pm", ".vbs",
    ".vba", ".bas", ".cls", ".ahk", ".ah2", ".r",
    // web
    ".html", ".htm", ".xhtml", ".css", ".scss", ".less", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".vue",
    ".svg", ".php",
    // code and project files
    ".c", ".h", ".cpp", ".hpp", ".cc", ".cxx", ".ino", ".cs", ".csproj", ".sln", ".props", ".targets", ".xaml",
    ".resx", ".java", ".kt", ".kts", ".gradle", ".groovy", ".swift", ".go", ".rs", ".dart", ".vb", ".tf",
    ".tfvars", ".hcl", ".cmake",
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
    // Copied under another name first: if that fails, the installed Slate is still as it was.
    let tmp = dir.join(format!("Slate.update-{}.exe", std::process::id()));
    // (On the disk before it takes the name: after a power cut a renamed file can come back empty.)
    if let Err(e) = std::fs::copy(&me, &tmp).and_then(|_| std::fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if dest.exists() {
        // Windows lets a running exe be renamed but not overwritten.
        let old = dir.join(format!("Slate.old-{}.exe", std::process::id()));
        if let Err(e) = std::fs::rename(&dest, &old) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            let _ = std::fs::rename(&old, &dest);
            return Err(e);
        }
    } else {
        std::fs::rename(&tmp, &dest)?;
    }
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
    let un = UNINSTALL;
    ok &= set(un, Some("DisplayName"), "Slate");
    ok &= set(un, Some("DisplayIcon"), &icon);
    ok &= set(un, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    ok &= set(un, Some("Publisher"), "Slate");
    let loc = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    ok &= set(un, Some("InstallLocation"), &loc);
    ok &= set(un, Some("UninstallString"), &format!("\"{e}\" --uninstall"));
    ok &= set(un, Some("QuietUninstallString"), &format!("\"{e}\" --uninstall --quiet"));
    let kb = std::fs::metadata(exe).map_or(0, |m| m.len().div_ceil(1024)) as u32;
    ok &= set_dword(un, "EstimatedSize", kb);
    // Win+R "slate"
    ok &= set(APP_PATH, None, &e);
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    ok
}

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate";
const APP_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths\Slate.exe";

fn set_dword(path: &str, name: &str, value: u32) -> bool {
    use windows::Win32::System::Registry::REG_DWORD;
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), 0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .is_err()
        {
            return false;
        }
        let ok = RegSetValueExW(key, &HSTRING::from(name), 0, REG_DWORD, Some(&value.to_le_bytes())).is_ok();
        let _ = RegCloseKey(key);
        ok
    }
}

/// After an update: the version Windows shows under Installed apps is this one's, if this is the Slate that was set
/// up there.
pub fn refresh_version() {
    use windows::Win32::System::Registry::{RRF_RT_REG_SZ, RegGetValueW};
    let read = |name: &str| -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(UNINSTALL),
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            )
        }
        .is_ok();
        ok.then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    };
    let Some(here) = super::settings::exe_dir() else { return };
    let ours = read("InstallLocation").is_some_and(|l| Path::new(&l) == here);
    if ours && read("DisplayVersion").as_deref() != Some(env!("CARGO_PKG_VERSION")) {
        set(UNINSTALL, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    }
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

/// `Slate.exe --uninstall` (Settings → Apps → Slate → Uninstall): removes what `make_default` added. Slate's folder
/// and its data folder stay; the message says where they are (`quiet`: no message, for `--uninstall --quiet`).
pub fn uninstall(quiet: bool) {
    use windows::Win32::System::Registry::RegDeleteTreeW;
    unsafe {
        for k in [
            format!(r"Software\Classes\{PROGID}"),
            r"Software\Classes\Applications\Slate.exe".to_string(),
            r"Software\Classes\*\shell\Slate".to_string(),
            r"Software\Slate".to_string(),
            UNINSTALL.to_string(),
            APP_PATH.to_string(),
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
        if quiet {
            return;
        }
        let folder = super::settings::exe_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let data = super::settings::data_dir().display().to_string();
        win::info(
            windows::Win32::Foundation::HWND::default(),
            "Slate",
            &format!(
                "Slate was removed from your account's settings. You can now delete its folder ({folder}).\n\nYour settings \
                 and unsaved text are in {data}; delete that folder too if you don't need them."
            ),
        );
    }
}
