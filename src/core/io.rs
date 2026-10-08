//! Opening and saving files.
//!
//! Small files (up to `MEM_LIMIT`) are read into memory and the file is closed again, like Notepad. Bigger files
//! are read on demand straight from disk, so even multi-GB files open instantly; their newline index is built in
//! the background. Files in UTF-16 or the ANSI code page are converted to UTF-8 when opened (big ones into a
//! self-deleting temp file) and converted back when saved.
//!
//! A file is only taken as ANSI if its text converts back to exactly its bytes; otherwise it stays UTF-8, where
//! every byte is kept as it is.
//!
//! Saving writes a hidden temp file next to the target and then swaps it into place with a POSIX-semantics
//! rename, which works even while Slate still reads the old file (big files, undo history): open handles keep
//! seeing the old content. Where that rename isn't supported (network shares, FAT drives) a plain rename is
//! tried, and for a file Slate still has open, ReplaceFile. A failed or cancelled save leaves the original
//! untouched. Saving refuses to write text that ANSI can't hold (unless asked to) and text read from a file
//! another program has written into meanwhile (see `Source::changed_in_place`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN as HIDDEN, FILE_BASIC_INFO, FILE_RENAME_INFO, FileBasicInfo, FileRenameInfoEx,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_IGNORE_ACL_ERRORS,
    REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW, SetFileAttributesW, SetFileInformationByHandle,
};
use windows::core::PCWSTR;

use super::buffer::{Buffer, Snapshot};
use super::document::{DiskInfo, Document};
use super::job::{Ctx, Job, Notify};
use super::source::{IndexBuilder, Source, create_temp_file};
use super::text::{
    self, AnsiCheck, AnsiDecoder, AnsiEncoder, Encoding, Utf16Decoder, Utf16Encoder, detect_encoding, detect_eol,
};

/// Files up to this size are read into memory.
pub const MEM_LIMIT: u64 = 64 << 20;
const SAMPLE: u64 = 1 << 20;
const CHUNK: u64 = 4 << 20;

pub enum Loading {
    /// Ready to use.
    Ready(Document),
    /// Usable now; the newline index is being built (see `Document::is_ready`).
    Indexing(Document, Job<bool>),
    /// A big file in another encoding is being converted first.
    Converting(Job<io::Result<Document>>),
}

/// Whether a file looks like a program, image or other binary rather than text: zero bytes near the start in
/// something that isn't UTF-16.
pub fn looks_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut head = vec![0u8; 8192];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = &head[..n];
    let utf16 = matches!(detect_encoding(head, n == 8192).0, Encoding::Utf16Le | Encoding::Utf16Be);
    !utf16 && memchr::memchr(0, head).is_some()
}

pub fn disk_info(path: &Path) -> Option<DiskInfo> {
    let m = fs::metadata(path).ok()?;
    Some(DiskInfo { len: m.len(), modified: m.modified().ok()? })
}

pub fn open(path: &Path, notify: Notify) -> io::Result<Loading> {
    open_with(path, notify, None, None)
}

/// Opens `path`. `prev` is the source of the same file opened earlier (a reload): if the file only grew, its
/// index is reused. `force` overrides the detected encoding.
pub fn open_with(path: &Path, notify: Notify, prev: Option<Arc<Source>>, force: Option<Encoding>) -> io::Result<Loading> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        return Err(io::Error::other("That is a folder, not a file."));
    }
    let disk = disk_info(path);
    if meta.len() <= MEM_LIMIT {
        let data = fs::read(path)?;
        let mut doc = document_from_bytes_as(data, force);
        doc.path = Some(path.to_path_buf());
        doc.disk = disk;
        return Ok(Loading::Ready(doc));
    }
    let src = Source::open_file(path)?;
    if let Some(prev) = prev {
        if prev.file_path() == Some(path) {
            src.reuse_index_from(&prev);
        }
    }
    let src = Arc::new(src);
    let mut sample = Vec::new();
    src.read_into(0, SAMPLE, &mut sample);
    let truncated = src.len() > SAMPLE;
    let (encoding, bom) = match force {
        Some(e) => (e, if sample.starts_with(e.bom()) { e.bom().len() } else { 0 }),
        None => match detect_encoding(&sample, truncated) {
            (Encoding::Ansi, _) if !text::ansi_fits(&sample, truncated) => (Encoding::Utf8, 0),
            d => d,
        },
    };
    if encoding.is_native() {
        let mut doc = Document::new_pending(src.clone(), bom as u64);
        doc.path = Some(path.to_path_buf());
        doc.encoding = encoding;
        doc.bom = bom > 0;
        doc.eol = detect_eol(&sample[bom..]);
        doc.disk = disk;
        let total = src.len();
        let job = Job::spawn(total, notify, move |ctx| src.build_index(&ctx.cancel, &ctx.progress));
        return Ok(Loading::Indexing(doc, job));
    }
    let path = path.to_path_buf();
    let total = src.len();
    Ok(Loading::Converting(Job::spawn(total, notify, move |ctx| {
        let mut doc = match convert_to_temp(&src, encoding, bom as u64, force.is_none(), ctx) {
            // Detected as ANSI, but further on it doesn't convert to text and back exactly: keep its bytes as they
            // are instead (UTF-8, read straight from the file like any big file).
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                if !src.build_index(&ctx.cancel, &ctx.progress) {
                    let why = src.index_error().unwrap_or_else(|| "cancelled".into());
                    return Err(io::Error::other(why));
                }
                let mut doc = Document::new_pending(src.clone(), 0);
                doc.eol = detect_eol(&sample);
                doc
            }
            r => r?,
        };
        doc.path = Some(path);
        doc.disk = disk;
        Ok(doc)
    })))
}

/// A document from a whole file's bytes (encoding detected, BOM removed, converted to UTF-8).
pub fn document_from_bytes(data: Vec<u8>) -> Document {
    document_from_bytes_as(data, None)
}

/// Like `document_from_bytes`, optionally forcing the encoding.
pub fn document_from_bytes_as(mut data: Vec<u8>, force: Option<Encoding>) -> Document {
    let (mut encoding, bom) = match force {
        Some(e) => (e, if data.starts_with(e.bom()) && !e.bom().is_empty() { e.bom().len() } else { 0 }),
        None => detect_encoding(&data, false),
    };
    let content = match encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => {
            data.drain(..bom);
            data
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let mut out = Vec::with_capacity(data.len());
            let mut d = Utf16Decoder::new(encoding == Encoding::Utf16Be);
            d.push(&data[bom..], &mut out);
            d.finish(&mut out);
            out
        }
        Encoding::Ansi => {
            let mut out = Vec::with_capacity(data.len() + data.len() / 8);
            let mut d = AnsiDecoder::new();
            d.push(&data, &mut out);
            d.finish(&mut out);
            if force.is_none() && !text::ansi_round_trips(&out, &data) {
                // Detected, but saving the text in ANSI wouldn't give these bytes back: keep them as they are.
                encoding = Encoding::Utf8;
                data
            } else {
                out
            }
        }
    };
    let eol = detect_eol(&content[..content.len().min(SAMPLE as usize)]);
    let nl = bytecount::count(&content, b'\n') as u64;
    let mut doc = Document::from_buffer(Buffer::from_source(Arc::new(Source::from_vec(content)), nl));
    doc.encoding = encoding;
    doc.bom = bom > 0;
    doc.eol = eol;
    doc
}

/// Converts a big file to UTF-8 in a temp file. `verify` (ANSI that was detected, not chosen): stop with
/// `InvalidData` as soon as the text doesn't convert back to exactly the file's bytes.
fn convert_to_temp(src: &Source, encoding: Encoding, bom: u64, verify: bool, ctx: &Ctx) -> io::Result<Document> {
    let mut check = (verify && encoding == Encoding::Ansi).then(AnsiCheck::new);
    let not_ansi = || io::Error::new(io::ErrorKind::InvalidData, "not text in the ANSI code page");
    let (file, temp_path) = create_temp_file()?;
    let mut idx = IndexBuilder::new();
    let mut out_len = 0u64;
    let mut first: Vec<u8> = Vec::new();
    {
        let mut w = BufWriter::with_capacity(1 << 20, &file);
        let mut emit = |out: &[u8]| -> io::Result<()> {
            if first.len() < SAMPLE as usize {
                first.extend_from_slice(&out[..out.len().min(SAMPLE as usize - first.len())]);
            }
            idx.push(out);
            out_len += out.len() as u64;
            w.write_all(out)
        };
        let mut u16d = Utf16Decoder::new(encoding == Encoding::Utf16Be);
        let mut ansi = AnsiDecoder::new();
        let mut out = Vec::with_capacity(CHUNK as usize * 2);
        let mut input = Vec::with_capacity(CHUNK as usize);
        let mut pos = bom;
        while pos < src.len() {
            if ctx.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            input.clear();
            src.read_into(pos, pos + CHUNK, &mut input);
            pos += input.len() as u64;
            out.clear();
            match encoding {
                Encoding::Ansi => ansi.push(&input, &mut out),
                _ => u16d.push(&input, &mut out),
            }
            if let Some(c) = check.as_mut() {
                c.push(&input, &out);
                if !c.ok() {
                    return Err(not_ansi());
                }
            }
            emit(&out)?;
            ctx.set(pos);
        }
        out.clear();
        match encoding {
            Encoding::Ansi => ansi.finish(&mut out),
            _ => u16d.finish(&mut out),
        }
        if check.take().is_some_and(|c| !c.finish(&out)) {
            return Err(not_ansi());
        }
        emit(&out)?;
        drop(emit);
        w.flush()?;
    }
    if src.read_errors() > 0 {
        return Err(io::Error::other("Part of the file couldn't be read."));
    }
    let nl = idx.newlines();
    let s = Arc::new(Source::from_file(file, out_len, temp_path, true, Some(idx.finish())));
    let mut doc = Document::from_buffer(Buffer::from_source(s, nl));
    doc.encoding = encoding;
    doc.bom = bom > 0;
    doc.eol = detect_eol(&first);
    Ok(doc)
}

// ---------------------------------------------------------------------------------------------------------------
// Saving

#[derive(Debug)]
pub enum SaveError {
    Cancelled,
    ReadOnly,
    /// Some characters can't be stored in the ANSI code page (they would become `?`); nothing was written.
    Lossy,
    /// Another program wrote into a file the text is read from (see `Source::changed_in_place`).
    Changed,
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Cancelled => write!(f, "Saving was cancelled."),
            SaveError::ReadOnly => write!(f, "The file is read-only."),
            SaveError::Lossy => write!(f, "Some characters can't be saved in ANSI."),
            SaveError::Changed => write!(
                f,
                "Another program changed this file while it was open. Slate reads big files from disk, so the parts \
                 you didn't edit aren't your version any more, and saving would mix the two. Nothing was saved."
            ),
            SaveError::Io(s) => write!(f, "{s}"),
        }
    }
}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        SaveError::Io(friendly_io(&e))
    }
}

pub fn friendly_io(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "Access was denied. The file may be open in another program, or you \
                                            may not have permission to change it."
            .into(),
        io::ErrorKind::NotFound => "The file or folder wasn't found.".into(),
        _ => match e.raw_os_error() {
            Some(32) | Some(33) => "Another program is using the file.".into(),
            Some(112) => "The disk is full.".into(),
            _ => e.to_string(),
        },
    }
}

pub struct Saved {
    pub disk: Option<DiskInfo>,
    /// For big files saved in UTF-8: the saved file as a source, where the text starts in it, and its newline
    /// count, so the document can switch to it (see `Document::rebase_on`).
    pub rebase: Option<(Arc<Source>, u64, u64)>,
    /// Some characters couldn't be represented in the ANSI code page and were saved as `?`.
    pub lossy: bool,
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE: u32 = 0x0001_0000;
const SHARE_READ_DELETE: u32 = 0x1 | 0x4;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const KEEP_ATTRIBUTES: u32 = 0x2 | 0x4 | 0x20 | 0x2000; // hidden, system, archive, not content indexed
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;

static SAVE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// The `\\?\` form of an absolute path, which isn't limited to 260 characters.
fn verbatim(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.as_os_str().to_string_lossy();
    if s.starts_with(r"\\?\") {
        abs
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{unc}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// Renames the open file `file` to `target`, replacing it even if it is open elsewhere (NTFS, Windows 10 1809+).
fn posix_rename(file: &File, target: &Path) -> io::Result<()> {
    let name: Vec<u16> = target.as_os_str().encode_wide().collect();
    let size = std::mem::size_of::<FILE_RENAME_INFO>() + name.len() * 2;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    unsafe {
        (*info).Anonymous.Flags = FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileRenameInfoEx,
            info as *const core::ffi::c_void,
            size as u32,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

/// Renames the closed file `from` to `to`, replacing it (fails if `to` is open, even with delete sharing).
fn move_file(from: &Path, to: &Path) -> bool {
    let (f, t) = (wide(&verbatim(from)), wide(&verbatim(to)));
    unsafe { MoveFileExW(PCWSTR(f.as_ptr()), PCWSTR(t.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .is_ok()
}

/// Puts the closed file `temp` in place of the existing `target` with ReplaceFile, which works where a rename
/// over a file that is still open doesn't (Slate reading a big file from a network share or a FAT drive): it
/// moves the old file aside under a backup name, then moves `temp` in. That isn't atomic, so it is only the last
/// thing tried; if it stops half way (the old file moved aside, the new one not in), the old one is moved back.
/// After it worked the backup (the old file, which open handles still read) is deleted. ReplaceFile also gives
/// the new file the old one's security settings, alternate streams and attributes.
fn replace_file(temp: &Path, target: &Path, dir: &Path) -> bool {
    let n = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let backup = dir.join(format!(".slate-bak-{}-{n}.tmp", std::process::id()));
    let (t, r, b) = (wide(&verbatim(target)), wide(&verbatim(temp)), wide(&verbatim(&backup)));
    let flags = REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS;
    let ok = unsafe { ReplaceFileW(PCWSTR(t.as_ptr()), PCWSTR(r.as_ptr()), PCWSTR(b.as_ptr()), flags, None, None) }
        .is_ok();
    if ok {
        // (it may stay in the folder until Slate lets go of it, on a network share: hidden)
        unsafe {
            let _ = SetFileAttributesW(PCWSTR(b.as_ptr()), HIDDEN);
        }
        let _ = fs::remove_file(&backup);
    } else if !target.exists() && backup.exists() {
        unsafe {
            let _ = MoveFileExW(PCWSTR(b.as_ptr()), PCWSTR(t.as_ptr()), MOVEFILE_WRITE_THROUGH);
        }
    }
    ok
}

/// Gives the new file the old one's creation time and attributes (and clears our temp "hidden" flag).
fn copy_identity(file: &File, old: Option<&fs::Metadata>) {
    let mut info = FILE_BASIC_INFO::default();
    let attrs = old.map_or(0, |m| m.file_attributes() & KEEP_ATTRIBUTES);
    info.FileAttributes = if attrs == 0 { FILE_ATTRIBUTE_NORMAL } else { attrs };
    if let Some(m) = old {
        info.CreationTime = m.creation_time() as i64;
    }
    unsafe {
        let _ = SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        );
    }
}

/// The byte order mark to write: the encoding's, if the document has one.
pub fn bom_bytes(encoding: Encoding, bom: bool) -> &'static [u8] {
    match encoding {
        Encoding::Utf8Bom => encoding.bom(),
        _ if bom => encoding.bom(),
        _ => &[],
    }
}

/// Writes the content (with BOM and encoding) to `w`; returns (index of the written bytes for UTF-8, lossy).
/// Stops with `Lossy` at the first character ANSI can't hold, unless `lossy_ok`.
fn write_content(
    snap: &Snapshot,
    encoding: Encoding,
    bom: bool,
    lossy_ok: bool,
    w: &mut dyn Write,
    ctx: &Ctx,
) -> Result<(Option<IndexBuilder>, bool), SaveError> {
    let bom = bom_bytes(encoding, bom);
    w.write_all(bom)?;
    let mut idx = encoding.is_native().then(IndexBuilder::new);
    if let Some(i) = idx.as_mut() {
        i.push(bom);
    }
    let mut u16e = Utf16Encoder::new(encoding == Encoding::Utf16Be);
    let mut ansi = AnsiEncoder::new();
    let mut out = Vec::new();
    let mut pos = 0u64;
    let mut err: Option<SaveError> = None;
    while pos < snap.len() {
        if ctx.cancelled() {
            return Err(SaveError::Cancelled);
        }
        let end = (pos + CHUNK).min(snap.len());
        snap.chunks(pos, end, &mut |c| {
            let r = match encoding {
                Encoding::Utf8 | Encoding::Utf8Bom => {
                    if let Some(i) = idx.as_mut() {
                        i.push(c);
                    }
                    w.write_all(c)
                }
                Encoding::Utf16Le | Encoding::Utf16Be => {
                    out.clear();
                    u16e.push(c, &mut out);
                    w.write_all(&out)
                }
                Encoding::Ansi => {
                    out.clear();
                    ansi.push(c, &mut out);
                    if ansi.lossy && !lossy_ok {
                        err = Some(SaveError::Lossy);
                        return false;
                    }
                    w.write_all(&out)
                }
            };
            if let Err(e) = r {
                err = Some(e.into());
                return false;
            }
            true
        });
        if let Some(e) = err.take() {
            return Err(e);
        }
        pos = end;
        ctx.set(pos);
    }
    out.clear();
    match encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => u16e.finish(&mut out),
        Encoding::Ansi => ansi.finish(&mut out),
        _ => {}
    }
    if ansi.lossy && !lossy_ok {
        return Err(SaveError::Lossy);
    }
    w.write_all(&out)?;
    Ok((idx, ansi.lossy))
}

/// Removes hidden temp files of saves that never finished (Slate was killed or crashed mid-save) in `dir`, and
/// old versions a `replace_file` couldn't delete.
fn clean_stale_temps(dir: &Path) {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let Ok(rd) = fs::read_dir(dir) else { return };
    let me = std::process::id();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name
            .strip_prefix(".slate-save-")
            .or_else(|| name.strip_prefix(".slate-bak-"))
            .and_then(|r| r.strip_suffix(".tmp"))
        else {
            continue;
        };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        // Only if that Slate is gone and the file is a few minutes old.
        let running = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map(|h| unsafe { windows::Win32::Foundation::CloseHandle(h) })
            .is_ok();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 300);
        if !running && old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Saves `snap` to `path` in `encoding` (with a byte order mark if `bom`). Run on a background thread. Text that
/// ANSI can't hold fails with `Lossy` (writing nothing) unless `lossy_ok`.
pub fn save(
    snap: &Snapshot,
    path: &Path,
    encoding: Encoding,
    bom: bool,
    lossy_ok: bool,
    ctx: &Ctx,
) -> Result<Saved, SaveError> {
    if snap.changed_in_place() {
        return Err(SaveError::Changed);
    }
    // Save through a symbolic link to the file it points at.
    let target: PathBuf = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let old = fs::metadata(&target).ok();
    if let Some(m) = &old {
        if m.is_dir() {
            return Err(SaveError::Io("That is a folder, not a file.".into()));
        }
        if m.permissions().readonly() {
            return Err(SaveError::ReadOnly);
        }
    }
    let errors_before = snap.read_errors();
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));

    // A short name (not built from the file's name, which could make the path too long).
    let temp = (0..50).find_map(|_| {
        let n = SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = dir.join(format!(".slate-save-{}-{n}.tmp", std::process::id()));
        // (std refuses create_new without write(true), even with an explicit access mode)
        OpenOptions::new()
            .read(true)
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .share_mode(SHARE_READ_DELETE)
            .create_new(true)
            .attributes(FILE_ATTRIBUTE_HIDDEN)
            .open(&p)
            .ok()
            .map(|f| (f, p))
    });

    // Never write over the original directly: a cancelled or failed save must leave it as it was.
    let Some((file, temp_path)) = temp else {
        return Err(SaveError::Io(
            "Slate couldn't create a temporary file in that folder (no permission?). Try Save As somewhere else."
                .into(),
        ));
    };

    let result = (|| -> Result<(Option<IndexBuilder>, bool), SaveError> {
        let mut w = BufWriter::with_capacity(1 << 20, &file);
        let r = write_content(snap, encoding, bom, lossy_ok, &mut w, ctx)?;
        w.flush()?;
        drop(w);
        if snap.read_errors() != errors_before {
            return Err(SaveError::Io(
                "Part of the original file couldn't be read (was it changed or removed?), so nothing was saved."
                    .into(),
            ));
        }
        // (again: another program may have written to it while this one read it)
        if snap.changed_in_place() {
            return Err(SaveError::Changed);
        }
        file.sync_all()?;
        copy_identity(&file, old.as_ref());
        Ok(r)
    })();
    let (idx, lossy) = match result {
        Ok(r) => r,
        Err(e) => {
            drop(file);
            let _ = fs::remove_file(&temp_path);
            return Err(e);
        }
    };

    // Swap the new file into place: a POSIX rename (atomic, and it replaces a file still open with delete
    // sharing, as Slate keeps big files); where the file system doesn't have those (network shares, FAT drives)
    // a plain rename, and if that can't replace the file because Slate still has it open, ReplaceFile.
    let posix = posix_rename(&file, &verbatim(&target)).is_ok();
    drop(file);
    let renamed = posix || move_file(&temp_path, &target) || (old.is_some() && replace_file(&temp_path, &target, dir));
    if !renamed {
        let _ = fs::remove_file(&temp_path);
        return Err(SaveError::Io(
            "Windows didn't let Slate replace the file (it may be in use). Try again, or use Save As.".into(),
        ));
    }

    clean_stale_temps(dir);
    let disk = disk_info(&target);
    let bom_len = bom_bytes(encoding, bom).len() as u64;
    let content_len = disk.map_or(0, |d| d.len).saturating_sub(bom_len);
    let rebase = match idx {
        Some(idx) if content_len > MEM_LIMIT && content_len == snap.len() => {
            let nl = idx.newlines();
            OpenOptions::new().read(true).share_mode(0x7).open(&target).ok().and_then(|f| {
                let len = f.metadata().ok()?.len();
                Some((Arc::new(Source::from_file(f, len, target.clone(), false, Some(idx.finish()))), bom_len, nl))
            })
        }
        _ => None,
    };
    Ok(Saved { disk, rebase, lossy })
}

/// Whether two paths name the same file (compares canonical paths).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn ctx() -> Ctx {
        Ctx { cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)) }
    }

    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("slate-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_replaces_a_file_we_still_read() {
        let dir = test_dir("replace");
        let path = dir.join("big.txt");
        let original: Vec<u8> = (0..200_000u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
        fs::write(&path, &original).unwrap();
        // Read it through an open file source, as for big files.
        let src = Arc::new(Source::open_file(&path).unwrap());
        assert!(src.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        let mut doc = Document::new_pending(src.clone(), 0);
        assert!(doc.is_ready());
        doc.begin(super::super::document::EditKind::Other, Default::default());
        doc.insert(0, b"edited\n");
        doc.end(Default::default());
        let snap = doc.snapshot();
        let saved = save(&snap, &path, Encoding::Utf8, false, false, &ctx()).unwrap();
        let on_disk = fs::read(&path).unwrap();
        assert_eq!(&on_disk[..7], b"edited\n");
        assert_eq!(&on_disk[7..], &original[..]);
        // The old source still reads the old content (and that file wasn't written into: replaced).
        let mut old = Vec::new();
        src.read_into(0, 12, &mut old);
        assert_eq!(&old, b"line 0\nline ");
        assert!(!src.changed_in_place());
        assert_eq!(saved.disk.unwrap().len, on_disk.len() as u64);
        // No temp files left behind.
        let left: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        drop(doc);
        drop(src);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn encodings_round_trip_through_save() {
        let dir = test_dir("enc");
        for enc in [Encoding::Utf8, Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be, Encoding::Ansi] {
            let path = dir.join(format!("{enc:?}.txt"));
            let mut doc = Document::from_text("Grüße, café\r\nline two\r\n".as_bytes());
            doc.encoding = enc;
            let snap = doc.snapshot();
            save(&snap, &path, enc, true, false, &ctx()).unwrap();
            let bytes = fs::read(&path).unwrap();
            let back = document_from_bytes(bytes);
            assert_eq!(back.encoding, enc, "{enc:?}");
            assert_eq!(back.read(0, back.len()), "Grüße, café\r\nline two\r\n".as_bytes(), "{enc:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_files_are_refused() {
        let dir = test_dir("ro");
        let path = dir.join("ro.txt");
        fs::write(&path, b"x").unwrap();
        let mut p = fs::metadata(&path).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(&path, p.clone()).unwrap();
        let mut doc = Document::from_text(b"y");
        let snap = doc.snapshot();
        assert!(matches!(save(&snap, &path, Encoding::Utf8, false, false, &ctx()), Err(SaveError::ReadOnly)));
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(&path, p).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_to_write_bytes_the_file_no_longer_has() {
        let dir = test_dir("shrunk");
        let path = dir.join("big.txt");
        let original: Vec<u8> = (0..30_000u32).flat_map(|i| format!("line {i:05}\n").into_bytes()).collect();
        fs::write(&path, &original).unwrap();
        let src = Arc::new(Source::open_file(&path).unwrap());
        assert!(src.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        let mut doc = Document::new_pending(src.clone(), 0);
        doc.begin(super::super::document::EditKind::Other, Default::default());
        doc.insert(0, b"edit\n");
        doc.end(Default::default());
        // Another program cuts the file in half; the view reads the missing tail (zeros, counted as errors).
        let f = OpenOptions::new().write(true).share_mode(0x7).open(&path).unwrap();
        f.set_len(original.len() as u64 / 2).unwrap();
        drop(f);
        let mid = doc.len() * 3 / 4;
        let tail = doc.read(mid, mid + 100);
        assert!(tail.iter().all(|&b| b == 0));
        let snap = doc.snapshot();
        let other = dir.join("copy.txt");
        let r = save(&snap, &other, Encoding::Utf8, false, false, &ctx());
        // (the file it reads from got shorter: changed in place, which is found before reading it)
        assert!(matches!(r, Err(SaveError::Changed)), "{:?}", r.as_ref().map(|_| ()));
        assert!(!other.exists());
        drop(doc);
        drop(src);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn utf16_without_bom_stays_without_bom() {
        let dir = test_dir("nobom");
        let path = dir.join("u16.txt");
        let bytes: Vec<u8> = "no byte order mark here\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        fs::write(&path, &bytes).unwrap();
        let mut doc = document_from_bytes(fs::read(&path).unwrap());
        assert_eq!(doc.encoding, Encoding::Utf16Le);
        assert!(!doc.bom);
        let snap = doc.snapshot();
        save(&snap, &path, doc.encoding, doc.bom, false, &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_file_names_save_safely() {
        let dir = test_dir("longname");
        let name = format!("{}.txt", "n".repeat(235));
        let path = dir.join(&name);
        if fs::write(&path, b"original").is_err() {
            return; // the file system won't take a name this long at all
        }
        let mut doc = Document::from_text(b"changed");
        let snap = doc.snapshot();
        save(&snap, &path, Encoding::Utf8, false, false, &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        // a cancelled save leaves the file alone
        let c = ctx();
        c.cancel.store(true, Ordering::Relaxed);
        let mut doc = Document::from_text(&vec![b'x'; 10 << 20]);
        let snap = doc.snapshot();
        assert!(save(&snap, &path, Encoding::Utf8, false, false, &c).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        let _ = fs::remove_dir_all(&dir);
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    /// A document over a file read from disk (as for big files), with "EDIT\n" typed at the start.
    fn edited(path: &Path) -> (Document, Arc<Source>) {
        let src = Arc::new(Source::open_file(path).unwrap());
        assert!(src.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        let mut doc = Document::new_pending(src.clone(), 0);
        doc.begin(super::super::document::EditKind::Other, Default::default());
        doc.insert(0, b"EDIT\n");
        doc.end(Default::default());
        (doc, src)
    }

    #[test]
    fn text_from_a_file_rewritten_in_place_is_never_saved() {
        let dir = test_dir("io-inplace");
        let path = dir.join("big.log");
        let lines = |tag: &str| -> Vec<u8> {
            (0..60_000u32).flat_map(|i| format!("{tag} line {i:06}\n").into_bytes()).collect()
        };
        let original = lines("old");
        fs::write(&path, &original).unwrap();
        let (mut doc, src) = edited(&path);
        // A log being written: fine, the text is what it was (saving it drops what came after).
        let mut f = OpenOptions::new().append(true).share_mode(0x7).open(&path).unwrap();
        f.write_all(b"one more line\n").unwrap();
        drop(f);
        let copy = dir.join("copy.log");
        save(&doc.snapshot(), &copy, Encoding::Utf8, false, false, &ctx()).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), [b"EDIT\n".as_slice(), &original].concat());
        // Rewritten by another program (same length): the text would mix both versions, so nothing is written.
        let rewritten = lines("new");
        fs::write(&path, &rewritten).unwrap();
        let snap = doc.snapshot();
        assert!(matches!(save(&snap, &path, Encoding::Utf8, false, false, &ctx()), Err(SaveError::Changed)));
        assert!(matches!(save(&snap, &copy, Encoding::Utf8, false, false, &ctx()), Err(SaveError::Changed)));
        assert_eq!(fs::read(&path).unwrap(), rewritten);
        assert_eq!(names(&dir), ["big.log", "copy.log"]);
        drop((doc, snap, src));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ansi_saves_that_would_lose_characters_stop_first() {
        if text::ansi_codepage() != 1252 {
            return; // (the characters below are about Windows-1252)
        }
        let dir = test_dir("lossy");
        let path = dir.join("ansi.txt");
        fs::write(&path, b"caf\xE9\r\n").unwrap();
        let mut doc = document_from_bytes(fs::read(&path).unwrap());
        assert_eq!(doc.encoding, Encoding::Ansi);
        doc.begin(super::super::document::EditKind::Other, Default::default());
        doc.insert(0, "→ ✓ ".as_bytes());
        doc.end(Default::default());
        let snap = doc.snapshot();
        assert!(matches!(save(&snap, &path, Encoding::Ansi, false, false, &ctx()), Err(SaveError::Lossy)));
        assert_eq!(fs::read(&path).unwrap(), b"caf\xE9\r\n");
        assert_eq!(names(&dir), ["ansi.txt"]);
        // When the user says so.
        let saved = save(&snap, &path, Encoding::Ansi, false, true, &ctx()).unwrap();
        assert!(saved.lossy);
        assert_eq!(fs::read(&path).unwrap(), b"? ? caf\xE9\r\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mostly_utf8_files_stay_utf8_byte_for_byte() {
        let dir = test_dir("mostly");
        let path = dir.join("t.txt");
        let mut bytes = "Grüße – naïve café ✓ 日本語\r\n".repeat(20).into_bytes();
        bytes.extend_from_slice(b"one stray byte: \xFF\r\n");
        fs::write(&path, &bytes).unwrap();
        let mut doc = document_from_bytes(fs::read(&path).unwrap());
        assert_eq!(doc.encoding, Encoding::Utf8);
        save(&doc.snapshot(), &path, doc.encoding, doc.bom, false, &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_file_puts_a_file_in_place_of_one_still_open() {
        let dir = test_dir("replacefile");
        let target = dir.join("t.txt");
        fs::write(&target, b"old content of the file").unwrap();
        // Slate reading it (a big file): open with delete sharing.
        let src = Source::open_file(&target).unwrap();
        let temp = dir.join("new.tmp");
        fs::write(&temp, b"NEW content").unwrap();
        assert!(replace_file(&temp, &target, &dir));
        assert_eq!(fs::read(&target).unwrap(), b"NEW content");
        let mut old = Vec::new();
        src.read_into(0, 23, &mut old);
        assert_eq!(old, b"old content of the file");
        drop(src);
        assert_eq!(names(&dir), ["t.txt"]);
        // Another program has it open without delete sharing: nothing can replace it, and nothing changes.
        let other = OpenOptions::new().read(true).share_mode(0x1).open(&target).unwrap();
        fs::write(&temp, b"newer").unwrap();
        assert!(!replace_file(&temp, &target, &dir));
        fs::remove_file(&temp).unwrap();
        let mut doc = Document::from_text(b"newer");
        assert!(save(&doc.snapshot(), &target, Encoding::Utf8, false, false, &ctx()).is_err());
        drop(other);
        assert_eq!(fs::read(&target).unwrap(), b"NEW content");
        assert_eq!(names(&dir), ["t.txt"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A file Slate reads from disk, saved on a network share (where the POSIX rename isn't supported and a plain
    /// one can't replace a file still open). Needs `SLATE_TEST_SHARE`: a folder on a share (e.g. reached through
    /// `\\localhost\C$\...`); skipped without it.
    #[test]
    fn files_read_from_a_share_save_over_it() {
        let Some(share) = std::env::var_os("SLATE_TEST_SHARE") else { return };
        let dir = PathBuf::from(share).join(format!("slate-test-{}-share", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        let original: Vec<u8> = (0..200_000u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
        fs::write(&path, &original).unwrap();
        let (mut doc, src) = edited(&path);
        save(&doc.snapshot(), &path, Encoding::Utf8, false, false, &ctx()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), [b"EDIT\n".as_slice(), &original].concat());
        let mut old = Vec::new();
        src.read_into(0, 12, &mut old);
        assert_eq!(&old, b"line 0\nline ");
        drop((doc, src));
        assert_eq!(names(&dir), ["big.log"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
