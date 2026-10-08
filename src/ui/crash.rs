//! Crash reports, without telemetry. A native crash (an access violation, a stack overflow…) writes a minidump next
//! to crash.log in the data folder; panics are logged there already (mod.rs). Nothing is sent anywhere: Help →
//! Report a problem opens GitHub's new-issue form filled in with the version and Windows build, and the user
//! reviews it, adds what happened and sends it themselves (attaching crash.log or a dump if they want).

use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpWithThreadInfo, MiniDumpWriteDump,
    SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent,
    WaitForSingleObject,
};

use super::settings::data_dir;

/// Crash dumps kept in the data folder (the oldest go first).
const KEEP: usize = 3;

/// The version, with the commit it was built from when known: "0.4.0 (c21cc39)".
pub fn version() -> String {
    match option_env!("SLATE_COMMIT") {
        Some(c) if !c.is_empty() => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
        _ => env!("CARGO_PKG_VERSION").to_string(),
    }
}

// The crashed thread hands its exception to a thread that's been waiting for it from the start: a crash can leave the
// crashed thread with almost no stack (an overflow), too little to write a dump.
static GO: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static DONE: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static INFO: AtomicPtr<EXCEPTION_POINTERS> = AtomicPtr::new(std::ptr::null_mut());
static THREAD: AtomicU32 = AtomicU32::new(0);

/// Writes a minidump when Slate crashes (call once, early).
pub fn install() {
    unsafe {
        let (Ok(go), Ok(done)) = (CreateEventW(None, false, false, None), CreateEventW(None, false, false, None)) else {
            return;
        };
        GO.store(go.0, Ordering::SeqCst);
        DONE.store(done.0, Ordering::SeqCst);
        let spawned = std::thread::Builder::new().name("slate-crash".into()).spawn(move || {
            let (go, done) = (HANDLE(GO.load(Ordering::SeqCst)), HANDLE(DONE.load(Ordering::SeqCst)));
            WaitForSingleObject(go, INFINITE);
            write_dump(INFO.load(Ordering::SeqCst), THREAD.load(Ordering::SeqCst));
            let _ = SetEvent(done);
        });
        if spawned.is_ok() {
            SetUnhandledExceptionFilter(Some(on_crash));
        }
    }
}

unsafe extern "system" fn on_crash(info: *const EXCEPTION_POINTERS) -> i32 {
    // Once: a crash while dumping just ends the process.
    let go = GO.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !go.is_null() {
        INFO.store(info as *mut _, Ordering::SeqCst);
        THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        unsafe {
            let _ = SetEvent(HANDLE(go));
            WaitForSingleObject(HANDLE(DONE.load(Ordering::SeqCst)), 15_000);
        }
    }
    // EXCEPTION_CONTINUE_SEARCH: Windows ends the process (and its error reporting runs) as it would have.
    0
}

fn write_dump(info: *mut EXCEPTION_POINTERS, thread: u32) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let name = format!("crash-{secs}.dmp");
    let (code, address) = unsafe {
        info.as_ref()
            .and_then(|i| i.ExceptionRecord.as_ref())
            .map_or((0, std::ptr::null_mut()), |r| (r.ExceptionCode.0 as u32, r.ExceptionAddress))
    };
    let written = std::fs::File::create(dir.join(&name)).is_ok_and(|f| {
        use std::os::windows::io::AsRawHandle;
        let ex = MINIDUMP_EXCEPTION_INFORMATION { ThreadId: thread, ExceptionPointers: info, ClientPointers: false.into() };
        unsafe {
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                HANDLE(f.as_raw_handle()),
                MiniDumpWithThreadInfo,
                (!info.is_null()).then_some(&ex as *const _),
                None,
                None,
            )
        }
        .is_ok()
    });
    if !written {
        let _ = std::fs::remove_file(dir.join(&name));
    }
    super::log_crash(&format!(
        "crashed: exception {code:#010x} at {address:?}{}",
        if written { format!("; dump: {name}") } else { String::new() }
    ));
    prune(&dir);
}

/// Keeps the newest `KEEP` dumps.
fn prune(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut dumps: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("crash-") && n.ends_with(".dmp"))
        .collect();
    // (crash-<unix seconds>.dmp: same length for centuries, so names sort by time)
    dumps.sort();
    for old in dumps.iter().rev().skip(KEEP) {
        let _ = std::fs::remove_file(dir.join(old));
    }
}

/// "Windows 11 (build 26100.1742)", from the registry.
pub fn windows_version() -> String {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{HSTRING, w};
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let text = |name: &str| -> Option<String> {
        let mut buf = [0u16; 64];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key, &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
        }
        .is_ok();
        ok.then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    };
    let number = |name: &str| -> Option<u32> {
        let mut v = 0u32;
        let mut len = 4u32;
        let ok = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key, &HSTRING::from(name), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut len))
        }
        .is_ok();
        ok.then_some(v)
    };
    let Some(build) = text("CurrentBuild") else { return "Windows".into() };
    // (Windows 11 still calls itself "Windows 10" in ProductName; the build number tells.)
    let name = if build.parse::<u32>().is_ok_and(|b| b >= 22000) { "Windows 11" } else { "Windows 10" };
    match number("UBR") {
        Some(ubr) => format!("{name} (build {build}.{ubr})"),
        None => format!("{name} (build {build})"),
    }
}

/// GitHub's new-issue form for Slate, filled in with what helps and nothing about the user or their files.
pub fn report_url() -> String {
    let body = format!(
        "**What happened?**\n\n\n**What did you expect?**\n\n\n**How to make it happen, if you know:**\n1. \n\n---\n\
         Slate {} on {}\n\n\
         If Slate crashed: crash.log, and any crash-*.dmp file, in Slate's settings folder (Help → Open settings \
         folder) help find out why. They can contain bits of the text you had open, so attach them only if that's \
         all right.",
        version(),
        windows_version()
    );
    format!("https://github.com/{}/issues/new?body={}", super::update::REPO, encode(&body))
}

/// Percent-encoding for a URL's query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_holds_the_version_and_no_paths() {
        let url = report_url();
        assert!(url.starts_with("https://github.com/jamesccupps/Slate/issues/new?body="));
        assert!(url.contains(&encode(env!("CARGO_PKG_VERSION"))));
        assert!(!url.contains("%3A%5C"), "no Windows paths");
        assert_eq!(encode("a b&c/ü"), "a%20b%26c%2F%C3%BC");
    }
}
