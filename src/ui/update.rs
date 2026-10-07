//! Updates from GitHub. Slate asks GitHub for the latest published release of `REPO` (at most once a day, and from
//! Help → Check for updates). A newer one shows up in the status bar. Updating downloads the release's Slate.exe,
//! checks it against the release's Slate.exe.sha256, moves the running exe aside (Windows lets a running program be
//! renamed but not overwritten; old copies are deleted on the next start), puts the new one in its place and
//! restarts. The session brings the tabs and their unsaved text back.
//!
//! Releases are made by the GitHub workflow in `.github/workflows/build.yml` when a `v*` tag is pushed (as drafts;
//! a release only counts once it's published).

use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::core::{HSTRING, PCWSTR, w};

use crate::core::job::Ctx;

pub const REPO: &str = "jamesccupps/Slate";
/// Downloads bigger than this aren't Slate (the exe is a few MB).
const MAX_EXE: usize = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `v1.2.3`, `1.2`, `1.2.3-beta` (the suffix is ignored).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let mut parts = s.split(['-', '+']).next()?.split('.');
        let mut num = || -> Option<u32> { parts.next().map_or(Some(0), |p| p.parse().ok()) };
        let v = Version(num()?, num()?, num()?);
        (!s.is_empty()).then_some(v)
    }

    /// This Slate's version (tests can pretend to be an older one).
    pub fn current() -> Version {
        std::env::var("SLATE_UPDATE_TEST_VERSION")
            .ok()
            .and_then(|v| Version::parse(&v))
            .or_else(|| Version::parse(env!("CARGO_PKG_VERSION")))
            .unwrap_or(Version(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's page (what's new).
    pub page: String,
    pub exe_url: String,
    /// Bytes (for progress).
    pub exe_size: u64,
    pub sha_url: Option<String>,
}

/// Reads the release from GitHub's answer to `releases/latest`.
pub fn parse_release(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|_| "GitHub sent something unexpected".to_string())?;
    let tag = v["tag_name"].as_str().ok_or("The latest release has no version")?;
    let version = Version::parse(tag).ok_or_else(|| format!("The latest release has an odd version ({tag})"))?;
    let assets = v["assets"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let asset = |name: &str| assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)));
    let url = |name: &str| asset(name).and_then(|a| a["browser_download_url"].as_str()).map(String::from);
    Ok(Release {
        version,
        page: v["html_url"].as_str().unwrap_or("").to_string(),
        exe_url: url("Slate.exe").ok_or("The latest release has no Slate.exe")?,
        exe_size: asset("Slate.exe").and_then(|a| a["size"].as_u64()).unwrap_or(0),
        sha_url: url("Slate.exe.sha256"),
    })
}

/// The SHA-256 at the start of a `sha256sum`-style line ("<64 hex digits>  Slate.exe").
pub fn parse_sha(text: &[u8]) -> Option<[u8; 32]> {
    let hex = text.trim_ascii_start().get(..64)?;
    let mut out = [0u8; 32];
    for (i, pair) in hex.chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }.is_ok().then_some(out)
}

// ---- HTTPS ----

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn net_error(e: windows::core::Error) -> String {
    // WinHTTP's own error codes (12xxx)
    match (e.code().0 & 0xFFFF) as u32 {
        12002 => "GitHub didn't answer in time".into(),
        12007 | 12029 => "GitHub can't be reached (no internet connection?)".into(),
        12175 | 12038 | 12044 | 12045 | 12169 => "The secure connection to GitHub failed".into(),
        n => format!("Couldn't reach GitHub (error {n})"),
    }
}

/// GETs an https `url` (following redirects, which release downloads go through) and passes the body to `sink`.
fn get(url: &str, accept: &str, ctx: Option<&Ctx>, sink: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    let rest = url.strip_prefix("https://").ok_or("Only https downloads are allowed")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = if path.is_empty() { "/" } else { path };
    unsafe {
        let agent = HSTRING::from(format!("Slate/{}", env!("CARGO_PKG_VERSION")));
        let session = Handle(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(net_error(windows::core::Error::from_win32()));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000);
        let conn = Handle(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(net_error(windows::core::Error::from_win32()));
        }
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            return Err(net_error(windows::core::Error::from_win32()));
        }
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0).map_err(net_error)?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(net_error)?;
        let mut status = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(net_error)?;
        match status {
            200 => {}
            404 => return Err("There's no published release on GitHub yet".into()),
            403 | 429 => return Err("GitHub is busy right now; try again later".into()),
            s => return Err(format!("GitHub answered with an error ({s})")),
        }
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if ctx.is_some_and(|c| c.cancelled()) {
                return Err("Cancelled".into());
            }
            let mut got = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got).map_err(net_error)?;
            if got == 0 {
                return Ok(());
            }
            sink(&buf[..got as usize])?;
        }
    }
}

fn get_all(url: &str, accept: &str, max: usize, ctx: Option<&Ctx>) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    get(url, accept, ctx, &mut |chunk| {
        body.extend_from_slice(chunk);
        if let Some(c) = ctx {
            c.set(body.len() as u64);
        }
        if body.len() > max { Err("The download is bigger than expected".into()) } else { Ok(()) }
    })?;
    Ok(body)
}

/// Asks GitHub for the latest published release.
pub fn latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    parse_release(&get_all(&url, "application/vnd.github+json", 1 << 20, None)?)
}

/// Downloads the release's Slate.exe next to the running one and checks it against the release's checksum.
/// Returns the downloaded file.
pub fn download(rel: &Release, ctx: &Ctx) -> Result<PathBuf, String> {
    let sha_url = rel.sha_url.as_deref().ok_or("The release has no checksum (Slate.exe.sha256), so it can't be checked")?;
    let want = parse_sha(&get_all(sha_url, "application/octet-stream", 4096, None)?).ok_or("The release's checksum can't be read")?;
    let exe = get_all(&rel.exe_url, "application/octet-stream", MAX_EXE, Some(ctx))?;
    if !exe.starts_with(b"MZ") || sha256(&exe) != Some(want) {
        return Err("The download doesn't match the release's checksum, so it wasn't installed".into());
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = me.parent().ok_or("Can't tell where Slate is")?;
    let tmp = dir.join(format!("Slate.update-{}.exe", std::process::id()));
    fs::write(&tmp, &exe).map_err(|e| format!("Can't write to {} ({e})", dir.display()))?;
    Ok(tmp)
}

/// Puts the downloaded exe in place of the running one, which is renamed aside.
pub fn install(new_exe: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let old = me.with_file_name(format!("Slate.old-{stamp}.exe"));
    if let Err(e) = fs::rename(&me, &old) {
        let _ = fs::remove_file(new_exe);
        return Err(format!("Couldn't move the running version aside ({e})"));
    }
    if let Err(e) = fs::rename(new_exe, &me) {
        let _ = fs::rename(&old, &me);
        let _ = fs::remove_file(new_exe);
        return Err(format!("Couldn't put the new version in place ({e})"));
    }
    Ok(())
}

/// Starts the (new) Slate; it waits for this one to finish closing.
pub fn restart() {
    if let Ok(me) = std::env::current_exe() {
        let _ = std::process::Command::new(me).arg("--wait-for").arg(std::process::id().to_string()).spawn();
    }
}

/// Started by `restart`: waits (a few seconds at most) for the old Slate to exit.
pub fn wait_for(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, 15_000);
            let _ = CloseHandle(h);
        }
    }
}

/// Opens the release's page in the browser.
pub fn show_page(rel: &Release) {
    if rel.page.starts_with("https://github.com/") {
        unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(rel.page.as_str()),
                None,
                None,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.2"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("2.0.0-beta.1"), Some(Version(2, 0, 0)));
        assert_eq!(Version::parse("vx"), None);
        assert_eq!(Version::parse(""), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn release_answer() {
        let json = br#"{"tag_name": "v0.3.1", "html_url": "https://github.com/jamesccupps/Slate/releases/tag/v0.3.1",
            "assets": [{"name": "Slate.exe.sha256", "browser_download_url": "https://github.com/x/Slate.exe.sha256"},
                       {"name": "Slate.exe", "browser_download_url": "https://github.com/x/Slate.exe"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, Version(0, 3, 1));
        assert_eq!(r.exe_url, "https://github.com/x/Slate.exe");
        assert_eq!(r.sha_url.as_deref(), Some("https://github.com/x/Slate.exe.sha256"));
        assert!(parse_release(br#"{"tag_name": "v1.0.0", "assets": []}"#).is_err());
        assert!(parse_release(b"<html>").is_err());
    }

    #[test]
    fn checksums() {
        let digest = sha256(b"abc").unwrap();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(parse_sha(format!("{hex}  Slate.exe\n").as_bytes()), Some(digest));
        assert_eq!(parse_sha(hex.to_uppercase().as_bytes()), Some(digest));
        assert_eq!(parse_sha(b"not a hash"), None);
    }
}
