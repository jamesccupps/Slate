//! What the engine asks the operating system for: opening files without getting in anyone's way, reading at an
//! offset, a second handle on an open file, a file's stamp and identity, self-deleting temp files, free space, and
//! whether a process is still running. Windows and Linux (any Unix) each have their own; the rest of the engine is
//! the same on both.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use super::source::{FileId, Stamp};

pub use imp::*;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, FileBasicInfo,
        GetFileInformationByHandle, GetFileInformationByHandleEx, ReOpenFile,
    };
    use windows::core::PCWSTR;

    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4; // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;

    /// Options for opening a file without blocking anyone else from reading, writing, renaming or deleting it.
    pub fn shared() -> OpenOptions {
        let mut o = OpenOptions::new();
        o.share_mode(SHARE_ALL);
        o
    }

    /// Reads into `buf` at `off` (how much it read: less at the end of the file).
    pub fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
        file.seek_read(buf, off)
    }

    /// Writes `buf` at `off` (how much it wrote).
    pub fn write_at(file: &File, buf: &[u8], off: u64) -> io::Result<usize> {
        file.seek_write(buf, off)
    }

    /// A second handle on the file `file` is open on (that very file, even if another one took its name since).
    pub fn reopen(file: &File) -> Option<File> {
        let h = HANDLE(file.as_raw_handle());
        let h = unsafe { ReOpenFile(h, GENERIC_READ.0, FILE_SHARE_MODE(SHARE_ALL), FILE_FLAGS_AND_ATTRIBUTES(0)) }.ok()?;
        Some(unsafe { File::from_raw_handle(h.0) })
    }

    /// The stamp and identity of the file `file` is open on (None if the file system won't say).
    pub fn stamp_of(file: &File) -> Option<(Stamp, FileId)> {
        let h = HANDLE(file.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let mut basic = FILE_BASIC_INFO::default();
        unsafe {
            GetFileInformationByHandle(h, &mut info).ok()?;
            GetFileInformationByHandleEx(
                h,
                FileBasicInfo,
                &mut basic as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
            .ok()?;
        }
        let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((
            Stamp { size, written: basic.LastWriteTime, changed: basic.ChangeTime },
            FileId { volume: info.dwVolumeSerialNumber, index },
        ))
    }

    /// The identity of the file at `path`, asking for its attributes only (no sharing conflicts with anyone).
    /// Ok(None): the file system doesn't say.
    pub fn id_at(path: &Path) -> io::Result<Option<FileId>> {
        let f = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(SHARE_ALL).open(path)?;
        Ok(stamp_of(&f).map(|s| s.1))
    }

    /// Creates `path` (which mustn't exist) as a file that deletes itself when its handle closes, also after a
    /// crash.
    pub fn create_self_deleting(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .attributes(FILE_ATTRIBUTE_TEMPORARY | FILE_ATTRIBUTE_HIDDEN)
            .open(path)
    }

    /// Bytes free for this user on the drive of `dir` (None if it doesn't say).
    pub fn free_space(dir: &Path) -> Option<u64> {
        let w: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut free = 0u64;
        unsafe {
            windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), None, None)
        }
        .ok()?;
        Some(free)
    }

    /// Whether the process `pid` is still running.
    pub fn process_running(pid: u32) -> bool {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map(|h| unsafe { windows::Win32::Foundation::CloseHandle(h) })
            .is_ok()
    }

    /// Whether `e` says the file or folder isn't found.
    pub fn is_not_found(e: &io::Error) -> bool {
        e.raw_os_error() == Some(2)
    }

    /// Whether `e` is one that may pass by itself, other than "not found": another program has the file, a network
    /// drive doesn't answer, a drive or folder isn't there (a USB stick that isn't plugged in).
    pub fn is_transient(e: &io::Error) -> bool {
        match e.raw_os_error() {
            // path not found, invalid drive, not ready, sharing / lock violation
            Some(3 | 15 | 21 | 32 | 33) => true,
            // the network ones
            Some(51 | 53..=55 | 59 | 64 | 65 | 67 | 121 | 1203 | 1222 | 1231 | 1232 | 1236 | 2250) => true,
            _ => matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::Interrupted),
        }
    }

    /// Why a read failed, in a few words, where it's about the file rather than the system's own message.
    pub fn read_failure(e: &io::Error) -> Option<&'static str> {
        match e.raw_os_error() {
            Some(33) => Some("another program has locked part of it"),
            Some(53 | 59 | 64 | 121 | 1231) => Some("the network drive stopped answering"),
            _ => None,
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};

    /// Options for opening a file (on Unix nobody is ever blocked by an open file).
    pub fn shared() -> OpenOptions {
        OpenOptions::new()
    }

    /// Reads into `buf` at `off` (how much it read: less at the end of the file).
    pub fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
        file.read_at(buf, off)
    }

    /// Writes `buf` at `off` (how much it wrote).
    pub fn write_at(file: &File, buf: &[u8], off: u64) -> io::Result<usize> {
        file.write_at(buf, off)
    }

    /// A second handle on the file `file` is open on (that very file, even if another one took its name since).
    /// (Reads at an offset don't wait for each other on one file here; a handle of its own all the same.)
    pub fn reopen(file: &File) -> Option<File> {
        file.try_clone().ok()
    }

    fn id_of(m: &std::fs::Metadata) -> FileId {
        let dev = m.dev();
        FileId { volume: (dev ^ (dev >> 32)) as u32, index: m.ino() }
    }

    /// The stamp and identity of the file `file` is open on (None if the file system won't say).
    pub fn stamp_of(file: &File) -> Option<(Stamp, FileId)> {
        let m = file.metadata().ok()?;
        // (in 100 ns units, as on Windows: the session keeps these, and an hour is the same number on both)
        let t = |s: i64, n: i64| s.saturating_mul(10_000_000).saturating_add(n / 100);
        Some((
            Stamp { size: m.len(), written: t(m.mtime(), m.mtime_nsec()), changed: t(m.ctime(), m.ctime_nsec()) },
            id_of(&m),
        ))
    }

    /// The identity of the file at `path`. Ok(None): the file system doesn't say.
    pub fn id_at(path: &Path) -> io::Result<Option<FileId>> {
        Ok(Some(id_of(&std::fs::metadata(path)?)))
    }

    /// Creates `path` (which mustn't exist) as a file only its handle can reach: its name is removed at once, so it
    /// goes away when the handle closes, also after a crash.
    pub fn create_self_deleting(path: &Path) -> io::Result<File> {
        let f = OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(path)?;
        std::fs::remove_file(path)?;
        Ok(f)
    }

    /// Bytes free for this user on the file system of `dir` (None if it doesn't say).
    pub fn free_space(dir: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
            return None;
        }
        let st = unsafe { st.assume_init() };
        Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
    }

    /// Whether the process `pid` is still running.
    pub fn process_running(pid: u32) -> bool {
        // (0: only whether it's there; a process of another user answers EPERM, and it's there too)
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Whether `e` says the file or folder isn't found.
    pub fn is_not_found(e: &io::Error) -> bool {
        e.raw_os_error() == Some(libc::ENOENT)
    }

    /// Whether `e` is one that may pass by itself, other than "not found": a network file system that doesn't
    /// answer, a device that isn't there, a file that's busy for a moment.
    pub fn is_transient(e: &io::Error) -> bool {
        match e.raw_os_error() {
            Some(
                libc::EIO
                | libc::EAGAIN
                | libc::EBUSY
                | libc::ETIMEDOUT
                | libc::EHOSTDOWN
                | libc::EHOSTUNREACH
                | libc::ENETDOWN
                | libc::ENETUNREACH
                | libc::ENETRESET
                | libc::ECONNABORTED
                | libc::ECONNRESET
                | libc::ENOTCONN
                | libc::ESTALE
                | libc::ENOMEDIUM
                | libc::ENXIO
                | libc::ENODEV,
            ) => true,
            _ => matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::Interrupted),
        }
    }

    /// Why a read failed, in a few words, where it's about the file rather than the system's own message.
    pub fn read_failure(e: &io::Error) -> Option<&'static str> {
        match e.raw_os_error() {
            Some(libc::ETIMEDOUT | libc::EHOSTDOWN | libc::EHOSTUNREACH | libc::ENOTCONN | libc::ESTALE) => {
                Some("the network drive stopped answering")
            }
            _ => None,
        }
    }
}
