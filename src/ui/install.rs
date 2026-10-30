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
/// Slate's own keys, named "Slate Text Editor" so another program called Slate can't take them over (0.15.0 and
/// older used plain "Slate": `move_names` moves them). The ProgID above stays: renaming it would undo the Default apps
/// choices people made for it, which only they can make again.
const APP_KEY: &str = r"Software\Slate Text Editor";
const CAPABILITIES: &str = r"Software\Slate Text Editor\Capabilities";
/// Slate's name in RegisteredApplications (and for Default apps' `registeredAppUser`, escaped).
const APP_NAME: &str = "Slate Text Editor";
/// "Edit with Slate" on every file's right-click menu.
const VERB: &str = r"Software\Classes\*\shell\Slate Text Editor";
/// What older Slates called those keys: its own key, its name in RegisteredApplications, "Edit with Slate", its
/// Installed apps entry. 0.15.0 and older used plain "Slate"; a test build of 0.16.0 its publisher's name.
const OLD_NAMES: [(&str, &str, &str, &str); 2] = [
    (r"Software\Slate", "Slate", r"Software\Classes\*\shell\Slate", r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate"),
    (
        r"Software\JamesCupps\Slate",
        "JamesCupps.Slate",
        r"Software\Classes\*\shell\JamesCupps.Slate",
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\JamesCupps.Slate",
    ),
];
const OLD_APP_KEY: &str = OLD_NAMES[0].0;
const OLD_VERB: &str = OLD_NAMES[0].2;
const OLD_UNINSTALL: &str = OLD_NAMES[0].3;
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
        ok &= set(&format!(r"{CAPABILITIES}\FileAssociations"), Some(ext), PROGID);
    }
    // Right-click any file: "Edit with Slate".
    ok &= set(VERB, None, "Edit with Slate");
    ok &= set(VERB, Some("Icon"), &icon);
    ok &= set(&format!(r"{VERB}\command"), None, &open);
    // Default apps (Settings) listing.
    ok &= set(CAPABILITIES, Some("ApplicationName"), "Slate");
    ok &= set(CAPABILITIES, Some("ApplicationDescription"), DESCRIPTION);
    ok &= set(r"Software\RegisteredApplications", Some(APP_NAME), CAPABILITIES);
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

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Slate Text Editor";
const DESCRIPTION: &str = "A fast, simple text editor that opens files of any size.";

const APP_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths\Slate.exe";

/// Moves what older Slates registered under other names (`OLD_NAMES`) to Slate's own (`APP_KEY`, `APP_NAME`, `VERB`,
/// `UNINSTALL`), once, and only what's Slate's: its Default apps entry, "Edit with Slate", its Installed apps entry,
/// and the note that `forget_unregistered` ran. The ProgID, "Open with" and the file types set to open with Slate
/// stay as they are.
pub fn move_names() {
    use windows::Win32::System::Registry::{RegCopyTreeW, RegDeleteTreeW, RegOpenKeyExW, KEY_READ};
    // Copies key `from` (all of it) to `to`, where there's nothing yet, then deletes `from`. Returns whether there was
    // one to move.
    let move_key = |from: &str, to: &str| unsafe {
        let mut src = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(from), 0, KEY_READ, &mut src).is_err() {
            return false;
        }
        let mut dst = HKEY::default();
        let mut copied = false;
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(to), 0, KEY_READ, &mut dst).is_ok() {
            // (moved before: the old one is only left over)
            let _ = RegCloseKey(dst);
            copied = true;
        } else if RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(to), 0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut dst, None)
            .is_ok()
        {
            copied = RegCopyTreeW(src, None, dst).is_ok();
            let _ = RegCloseKey(dst);
        }
        let _ = RegCloseKey(src);
        if copied {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(from));
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(from));
        }
        true
    };
    let mut moved = false;
    for (old_key, old_name, old_verb, old_uninstall) in OLD_NAMES {
        // Slate's own key (its Default apps capabilities, the Registration note): Slate's when it describes Slate or
        // holds only that note.
        let old_caps = format!(r"{old_key}\Capabilities");
        let ours = read_str(&old_caps, "ApplicationDescription").as_deref() == Some(DESCRIPTION)
            || (read_str(&old_caps, "ApplicationName").is_none() && read_dword(old_key, "Registration").is_some());
        if ours {
            let listed = read_str(r"Software\RegisteredApplications", old_name).is_some_and(|v| v.eq_ignore_ascii_case(&old_caps));
            moved |= move_key(old_key, APP_KEY);
            if listed {
                set(r"Software\RegisteredApplications", Some(APP_NAME), CAPABILITIES);
                delete_value(r"Software\RegisteredApplications", old_name);
            }
        }
        // "Edit with Slate": Slate's when it runs a Slate.exe
        if read_str(&format!(r"{old_verb}\command"), "").is_some_and(|c| c.to_ascii_lowercase().contains("slate.exe\" \"%1\"")) {
            moved |= move_key(old_verb, VERB);
        }
        // Installed apps: Slate's when it uninstalls with a Slate.exe --uninstall
        if read_str(old_uninstall, "UninstallString").is_some_and(|u| u.to_ascii_lowercase().contains("slate.exe\" --uninstall")) {
            moved |= move_key(old_uninstall, UNINSTALL);
        }
    }
    // (the test build's publisher key, if nothing else is in it)
    unsafe {
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, w!(r"Software\JamesCupps"));
    }
    if moved {
        unsafe {
            SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
        }
    }
}

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
    read_str(r"Software\RegisteredApplications", APP_NAME).is_some()
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

/// The usual kinds of files, whose choice setting up reports (`choices_note`).
const USUAL: &[&str] = &[".txt", ".log", ".json", ".xml", ".md", ".csv", ".ini", ".yaml", ".yml", ".cfg", ".conf"];

/// The app chosen for a kind of file, as Windows keeps it (read only: Windows lets only the person choose).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Slate,
    Other,
    /// None chosen: Windows asks "How do you want to open this?" the first time one is opened, also where Default
    /// apps shows Slate for it (the only app registered for it).
    NotYet,
}

/// The app chosen for `ext` (newer Windows 11 keeps it in UserChoiceLatest, else in UserChoice).
fn choice_of(ext: &str) -> Choice {
    let base = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}");
    let latest = format!(r"{base}\UserChoiceLatest");
    let progid = read_str(&latest, "ProgId")
        .or_else(|| read_str(&format!(r"{latest}\ProgId"), "ProgId"))
        .or_else(|| read_str(&format!(r"{base}\UserChoice"), "ProgId"));
    match progid {
        Some(p) if is_slate(&p) => Choice::Slate,
        Some(_) => Choice::Other,
        None => Choice::NotYet,
    }
}

/// What setting up says about the usual kinds of files: which open with Slate, which Windows still asks about the
/// first time, and which another app opens. (Default apps shows Slate for a kind of file no app was chosen for, which
/// looked done when it wasn't.)
pub fn choices_note(choices: &[(&str, Choice)]) -> String {
    let of = |c: Choice| choices.iter().filter(|(_, k)| *k == c).map(|(e, _)| *e).collect::<Vec<_>>().join(", ");
    let (slate, not_yet, other) = (of(Choice::Slate), of(Choice::NotYet), of(Choice::Other));
    if not_yet.is_empty() && other.is_empty() {
        return format!("Slate is set up, and opens the usual kinds of files ({slate}).");
    }
    let mut s = String::from("Slate is set up.");
    if !slate.is_empty() {
        s.push_str(&format!(" Opening with Slate: {slate}."));
    }
    if !not_yet.is_empty() {
        s.push_str(&format!(
            " Windows asks the first time you open {not_yet}: choose Slate and \"Always\" (or click them on the Default \
             apps page, even where it shows Slate already)."
        ));
    }
    if !other.is_empty() {
        s.push_str(&format!(" Another app opens {other}: to change that, click them on the Default apps page."));
    }
    s
}

/// Takes back what 0.8.1 and older registered for the kinds of files in `NOT_REGISTERED`, and a choice of Slate for
/// .bat and .cmd files (made in the box Windows showed because of it: they opened in Slate instead of running).
/// Once: `Registration` in Slate's key notes it.
fn forget_unregistered() {
    if read_dword(APP_KEY, "Registration").is_some_and(|v| v >= 2) {
        return;
    }
    for ext in NOT_REGISTERED {
        delete_value(&format!(r"Software\Classes\{ext}\OpenWithProgids"), PROGID);
        delete_value(r"Software\Classes\Applications\Slate.exe\SupportedTypes", ext);
        delete_value(&format!(r"{CAPABILITIES}\FileAssociations"), ext);
    }
    for ext in [".bat", ".cmd"] {
        forget_choice(ext);
    }
    set_dword(APP_KEY, "Registration", 2);
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
            VERB.to_string(),
            CAPABILITIES.to_string(),
            // (what 0.15.0 and older called them, if they're still there)
            OLD_VERB.to_string(),
            format!(r"{OLD_APP_KEY}\Capabilities"),
        ] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
        }
    }
    delete_value(r"Software\RegisteredApplications", APP_NAME);
    if read_str(r"Software\RegisteredApplications", "Slate").is_some_and(|v| v.eq_ignore_ascii_case(&format!(r"{OLD_APP_KEY}\Capabilities"))) {
        delete_value(r"Software\RegisteredApplications", "Slate");
    }
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
         \"Edit with Slate\" when you right-click a file.\n\nWindows only lets you choose the app for a kind of file \
         yourself: the first time you open one, Windows asks; choose Slate and \"Always\". Or click the kinds of files on \
         Windows' Default apps page, which opens next (it may show Slate before it's chosen)."
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
        ShellExecuteW(hwnd, w!("open"), w!("ms-settings:defaultapps?registeredAppUser=Slate%20Text%20Editor"), None, None, SW_SHOWNORMAL);
    }
    let choices: Vec<(&str, Choice)> = USUAL.iter().map(|&e| (e, choice_of(e))).collect();
    let note = choices_note(&choices);
    let mut a = cell.borrow_mut();
    let i = a.active;
    a.tabs[i].notice = Some(super::app::Notice {
        kind: super::app::NoticeKind::Info,
        text: note,
        actions: vec![("Dismiss".into(), super::app::NoticeAction::Dismiss)],
    });
    a.layout();
    a.invalidate();
}

/// `Slate.exe --install` (winget, scripts): installs Slate for the current user the way "Open files with Slate…"
/// does (its folder, the Start menu, "Open with", Installed apps), with nothing shown and Default apps not opened.
/// True when all of it worked (the exit code says so).
pub fn install_quiet() -> bool {
    // (a winget upgrade from 0.15.0 or older: its keys under the new names, not twice in Installed apps)
    move_names();
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
        for k in [APP_KEY, UNINSTALL, APP_PATH] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k));
        }
        // (what 0.15.0 and older called them)
        if read_dword(OLD_APP_KEY, "Registration").is_some() || read_str(&format!(r"{OLD_APP_KEY}\Capabilities"), "ApplicationDescription").as_deref() == Some(DESCRIPTION) {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(OLD_APP_KEY));
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(OLD_APP_KEY));
        }
        if read_str(OLD_UNINSTALL, "UninstallString").is_some_and(|u| u.to_ascii_lowercase().contains("slate.exe\" --uninstall")) {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(OLD_UNINSTALL));
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
    // A running exe can be renamed: under a name of its own, a Slate.exe installed again while this one is still
    // there (winget uninstall, then install) isn't the one deleted.
    let own = dir.join(format!("Slate.old-{}.exe", std::process::id()));
    let exe = if std::fs::rename(exe, &own).is_ok() { own } else { exe.to_path_buf() };
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = cleanup_command(&exe, dir).creation_flags(CREATE_NO_WINDOW).spawn();
}

/// The hidden `cmd` that deletes `exe`, the leftovers of updates beside it and the folder `dir`. The paths go in
/// through environment variables, read with delayed expansion (`/v:on`, `!VAR!`): that happens after cmd has
/// read the line and put in the loop's `%i`, so a `%`, `!`, `^`, `&` or `%i` in a folder's name is just a character.
fn cleanup_command(exe: &Path, dir: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let script = "for /l %i in (1,1,30) do @(if exist \"!SLATE_EXE!\" (del /f /q \"!SLATE_EXE!\" 2>nul & \
                  ping -n 2 127.0.0.1 >nul)) & del /f /q \"!SLATE_DIR!\\Slate.old-*.exe\" \"!SLATE_DIR!\\Slate.update-*.exe\" \
                  2>nul & rd \"!SLATE_DIR!\" 2>nul";
    let system = std::env::var_os("SystemRoot").map_or(PathBuf::from(r"C:\Windows"), PathBuf::from);
    let mut c = std::process::Command::new(system.join(r"System32\cmd.exe"));
    // (cmd takes away the outer quotes)
    c.raw_arg(format!("/d /v:on /c \"{script}\"")).env("SLATE_EXE", exe).env("SLATE_DIR", dir).current_dir(&system);
    c
}

#[cfg(test)]
mod choice_tests {
    use super::*;

    #[test]
    fn setting_up_says_which_kinds_of_files_are_chosen() {
        use Choice::*;
        assert_eq!(choices_note(&[(".txt", Slate), (".log", Slate)]), "Slate is set up, and opens the usual kinds of files (.txt, .log).");
        let s = choices_note(&[(".txt", Slate), (".json", NotYet), (".md", NotYet), (".csv", Other)]);
        assert!(s.starts_with("Slate is set up. Opening with Slate: .txt. Windows asks the first time you open .json, .md:"), "{s}");
        assert!(s.ends_with("Another app opens .csv: to change that, click them on the Default apps page."), "{s}");
        let s = choices_note(&[(".txt", NotYet)]);
        assert!(!s.contains("Opening with Slate") && !s.contains("Another app"), "{s}");
        // the usual kinds are all ones Slate registers for
        assert!(USUAL.iter().all(|e| registered().any(|r| r == *e)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_takes_any_folder_name_literally() {
        // (a name cmd would otherwise read: a variable, the loop's own, an escape, a command separator)
        let dir = std::env::temp_dir().join(format!("slate-cleanup {} %i 50% !x! ^a &b (c) %PATH%", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Slate.old-1.exe");
        for f in [&exe, &dir.join("Slate.update-2.exe")] {
            std::fs::write(f, b"MZ").unwrap();
        }
        let done = cleanup_command(&exe, &dir).status().unwrap();
        assert!(done.success() || !dir.exists());
        assert!(!dir.exists(), "{} is still there", dir.display());
        // a folder with something else in it stays
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&exe, b"MZ").unwrap();
        std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
        cleanup_command(&exe, &dir).status().unwrap();
        assert!(!exe.exists() && dir.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
