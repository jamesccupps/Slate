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
    ".tfvars", ".hcl", ".cmake", ".scala", ".sbt", ".m", ".mm", ".gcode", ".gco", ".ngc", ".cnc", ".iss", ".isl",
    ".nsi", ".nsh", ".ppcl",
];

/// Kinds of files Slate colors but doesn't register for: what Windows runs when it's double-clicked (or merges into
/// the registry: .reg), and .ts, as often a video as TypeScript. Registered for them, Slate made Windows ask "How do
/// you want to open this file?" with Slate offered and the way such a file opened before nowhere in the list (0.8.1
/// and older). "Edit with Slate" on the right-click menu still opens them.
const NOT_REGISTERED: &[&str] =
    &[".bat", ".cmd", ".vbs", ".js", ".reg", ".py", ".pyw", ".rb", ".pl", ".ahk", ".ah2", ".sh", ".bash", ".ts"];

/// The kinds of files Slate registers for.
fn registered() -> impl Iterator<Item = &'static str> {
    EXTENSIONS.iter().copied().filter(|e| !NOT_REGISTERED.contains(e))
}

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
    for ext in registered() {
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
    // (the name Slate.exe is signed with)
    ok &= set(un, Some("Publisher"), "James Cupps");
    let loc = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    ok &= set(un, Some("InstallLocation"), &loc);
    ok &= set(un, Some("UninstallString"), &format!("\"{e}\" --uninstall"));
    ok &= set(un, Some("QuietUninstallString"), &format!("\"{e}\" --uninstall --quiet"));
    let kb = std::fs::metadata(exe).map_or(0, |m| m.len().div_ceil(1024)) as u32;
    ok &= set_dword(un, "EstimatedSize", kb);
    // Win+R "slate"
    ok &= set(APP_PATH, None, &e);
    // (what 0.8.1 and older registered for the kinds of files Slate leaves alone now)
    forget_unregistered();
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    ok
}

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate";
const APP_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths\Slate.exe";

fn read_str(path: &str, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{RRF_RT_REG_SZ, RegGetValueW};
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    }
    .is_ok();
    ok.then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
}

fn read_dword(path: &str, name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::{RRF_RT_REG_DWORD, RegGetValueW};
    let mut v = 0u32;
    let mut len = 4u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut len),
        )
    }
    .is_ok();
    ok.then_some(v)
}

fn delete_value(path: &str, name: &str) {
    use windows::Win32::System::Registry::{RegDeleteValueW, RegOpenKeyExW};
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), 0, KEY_WRITE, &mut key).is_ok() {
            let _ = RegDeleteValueW(key, &HSTRING::from(name));
            let _ = RegCloseKey(key);
        }
    }
}

/// Deletes a key and what's in it. One Windows guards against changes (a file type's UserChoice) can still be
/// deleted whole: RegDeleteKey needs no right to change its values, RegDeleteTree (which empties it first) does.
fn delete_key(path: &str) {
    use windows::Win32::System::Registry::{RegDeleteKeyW, RegDeleteTreeW};
    let p = HSTRING::from(path);
    unsafe {
        if RegDeleteKeyW(HKEY_CURRENT_USER, &p).is_err() {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &p);
        }
    }
}

/// Whether "Open files with Slate…" set Slate up for files (Default apps lists it).
pub fn associated() -> bool {
    read_str(r"Software\RegisteredApplications", "Slate").is_some()
}

fn is_slate(progid: &str) -> bool {
    progid == PROGID || progid.eq_ignore_ascii_case(r"Applications\Slate.exe")
}

/// Where Slate is the app chosen for `ext`, the choice goes: such files open the way Windows opens them without one
/// again (a .bat runs), or Windows asks.
fn forget_choice(ext: &str) {
    let base = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}");
    let choice = format!(r"{base}\UserChoice");
    if read_str(&choice, "ProgId").is_some_and(|p| is_slate(&p)) {
        delete_key(&choice);
    }
    // (where newer Windows 11 keeps it)
    let latest = format!(r"{base}\UserChoiceLatest");
    if read_str(&latest, "ProgId").or_else(|| read_str(&format!(r"{latest}\ProgId"), "ProgId")).is_some_and(|p| is_slate(&p)) {
        delete_key(&format!(r"{latest}\ProgId"));
        delete_key(&latest);
    }
}

/// Takes back what 0.8.1 and older registered for the kinds of files in `NOT_REGISTERED`, and a choice of Slate for
/// .bat and .cmd files (made in the box Windows showed because of it: they opened in Slate instead of running).
/// Once: `Software\Slate\Registration` notes it.
fn forget_unregistered() {
    if read_dword(r"Software\Slate", "Registration").is_some_and(|v| v >= 2) {
        return;
    }
    for ext in NOT_REGISTERED {
        delete_value(&format!(r"Software\Classes\{ext}\OpenWithProgids"), PROGID);
        delete_value(r"Software\Classes\Applications\Slate.exe\SupportedTypes", ext);
        delete_value(r"Software\Slate\Capabilities\FileAssociations", ext);
    }
    for ext in [".bat", ".cmd"] {
        forget_choice(ext);
    }
    set_dword(r"Software\Slate", "Registration", 2);
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// Help → Stop opening files with Slate… (and `Slate.exe --unassociate`): takes back what "Open files with Slate…"
/// set up for files: Slate in "Open with", "Edit with Slate", its Default apps entry, and every file type set to
/// open with Slate, which opens the way Windows opens it without a choice again (or Windows asks). Slate stays
/// installed: its folder, the Start menu, Installed apps.
pub fn unassociate() {
    use windows::Win32::System::Registry::RegDeleteTreeW;
    for ext in EXTENSIONS {
        delete_value(&format!(r"Software\Classes\{ext}\OpenWithProgids"), PROGID);
        forget_choice(ext);
    }
    unsafe {
        for k in [
            format!(r"Software\Classes\{PROGID}"),
            r"Software\Classes\Applications\Slate.exe".to_string(),
            r"Software\Classes\*\shell\Slate".to_string(),
            r"Software\Slate\Capabilities".to_string(),
        ] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
        }
    }
    delete_value(r"Software\RegisteredApplications", "Slate");
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// Help → Stop opening files with Slate…: asks, then `unassociate`.
pub fn stop_default(cell: &Cell) {
    let hwnd = cell.borrow().hwnd;
    let detail = "Slate comes off \"Open with\", the right-click menu and Default apps, and the file types you set to \
                  open with Slate open the way they did without it (or Windows asks which app to use). Slate stays \
                  installed, with your settings and tabs; Help → Open files with Slate… sets it up again.";
    if win::ask(hwnd, "Slate", "Stop opening files with Slate?", detail, &["&Stop", "Cancel"]) != Some(0) {
        return;
    }
    unassociate();
    cell.borrow_mut().flash("Slate no longer opens files by itself. Help → Open files with Slate… sets it up again.", false);
}

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
    // 0.8.1 and older registered for kinds of files Slate leaves alone now (.bat...): taken back, once.
    if associated() {
        forget_unregistered();
    }
    let read = |name: &str| read_str(UNINSTALL, name);
    let Some(here) = super::settings::exe_dir() else { return };
    let ours = read("InstallLocation").is_some_and(|l| Path::new(&l) == here);
    if ours && read("DisplayVersion").as_deref() != Some(env!("CARGO_PKG_VERSION")) {
        set(UNINSTALL, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    }
    // (0.7.0 and older wrote "Slate")
    if ours && read("Publisher").as_deref() != Some("James Cupps") {
        set(UNINSTALL, Some("Publisher"), "James Cupps");
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
    if win::ask(hwnd, "Slate", "Open your text files with Slate?", &detail, &["&Set up", "Cancel"]) != Some(0) {
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

/// `Slate.exe --install` (winget, scripts): installs Slate for the current user the way "Open files with Slate…"
/// does (its folder, the Start menu, "Open with", Installed apps), with nothing shown and Default apps not opened.
/// True when all of it worked (the exit code says so).
pub fn install_quiet() -> bool {
    match install_exe() {
        Ok(exe) => {
            start_menu_shortcut(&exe);
            register(&exe)
        }
        Err(_) => false,
    }
}

/// `Slate.exe --uninstall` (Settings → Apps → Slate → Uninstall): removes what `make_default` added. Slate's folder
/// and its data folder stay; the message says where they are (`quiet`: no message, for `--uninstall --quiet`).
pub fn uninstall(quiet: bool) {
    use windows::Win32::System::Registry::RegDeleteTreeW;
    // (the file types set to open with Slate open the way Windows opens them without a choice again)
    unassociate();
    unsafe {
        for k in [r"Software\Slate".to_string(), UNINSTALL.to_string(), APP_PATH.to_string()] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let _ = std::fs::remove_file(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Slate.lnk"));
        }
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
        let installed = installed_copy();
        if !quiet {
            let data = super::settings::data_dir().display().to_string();
            let first = if installed.is_some() {
                "Slate was removed.".to_string()
            } else {
                let folder = super::settings::exe_dir().map(|d| d.display().to_string()).unwrap_or_default();
                format!("Slate was removed from your account's settings. You can now delete its folder ({folder}).")
            };
            win::info(
                windows::Win32::Foundation::HWND::default(),
                "Slate",
                &format!("{first}\n\nYour settings and unsaved text are in {data}; delete that folder too if you don't need them."),
            );
        }
        if let Some(exe) = installed {
            remove_after_exit(&exe);
        }
    }
}

/// The running exe, when it's the copy setup put in `%LOCALAPPDATA%\Programs\Slate` (winget's, or Help → Open
/// files with Slate…'s). A copy anywhere else (a portable one, a download) is the user's own.
fn installed_copy() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let installed = install_dir().join("Slate.exe");
    (installed.exists() && crate::core::io::same_file(&exe, &installed)).then_some(installed)
}

/// Deletes `exe` (and leftovers of updates beside it, and its folder if nothing else is in it) once this Slate has
/// ended: a running exe can't delete itself, so a hidden `cmd` tries again every second for half a minute.
fn remove_after_exit(exe: &Path) {
    let Some(dir) = exe.parent() else { return };
    let (exe, dir) = (exe.display(), dir.display());
    let script = format!(
        "for /l %i in (1,1,30) do @(if exist \"{exe}\" (del /f /q \"{exe}\" 2>nul & ping -n 2 127.0.0.1 >nul)) & \
         del /f /q \"{dir}\\Slate.old-*.exe\" \"{dir}\\Slate.update-*.exe\" 2>nul & rd \"{dir}\" 2>nul"
    );
    let system = std::env::var_os("SystemRoot").map_or(PathBuf::from(r"C:\Windows"), PathBuf::from);
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // (cmd takes away the outer quotes)
    let _ = std::process::Command::new(system.join(r"System32\cmd.exe"))
        .raw_arg(format!("/d /c \"{script}\""))
        .current_dir(&system)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
