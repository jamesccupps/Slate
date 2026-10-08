//! Immutable byte sources that documents are built from: a file on disk (read on demand through a small block
//! cache, never loaded whole) or a block of memory. Every source has a newline index (a count per 64 KiB block)
//! so line lookups stay fast on multi-GB files.
//!
//! Reads never fail from the caller's point of view: if the file shrank or became unreadable, the missing bytes
//! read as zeros and `read_errors` goes up. The UI shows a warning; anything that writes data out (save) checks
//! the counter and refuses to write zeros to disk.
//!
//! A file source also keeps what its file looked like through its own handle when it was opened (`Stamp`).
//! Another program writing into that very file (rather than putting a new file in its place, which leaves the
//! handle on the old one) means the parts of a document still read from it aren't what the user had any more:
//! `changed_in_place` tells, and saving refuses to mix the two. The index is only ever built from bytes that
//! were read: a read that keeps failing stops it (`index_error`), so line commands never trust a guessed count.
//!
//! Reading a lot at once (the index, hashing, big ranges for search and saving) goes through a second handle on
//! the same file (`bulk`): Windows runs the reads of one handle one after the other, so on a slow network drive the
//! window's block reads would otherwise wait behind them.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, FileBasicInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, ReOpenFile,
};

pub const BLOCK: u64 = 64 * 1024;
const CACHE_BLOCKS: usize = 512; // 32 MiB per file source
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4; // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
/// Complete blocks a fingerprint samples, spread over the file (plus its last complete block).
const SAMPLES: u64 = 32;
/// Waits before trying a failed index read again (a network drive that dropped for a moment, a region another
/// program had locked).
const RETRY_MS: [u64; 3] = if cfg!(test) { [60, 120, 240] } else { [100, 500, 2000] };

/// Newline index: `cum[i]` is the number of `\n` bytes before block `i`. Blocks `0..cum.len()-1` are indexed;
/// while a file is still being indexed in the background only a prefix is.
#[derive(Clone, Default)]
pub struct NlIndex {
    cum: Vec<u64>,
    complete: bool,
}

impl NlIndex {
    fn empty() -> Self {
        NlIndex { cum: vec![0], complete: false }
    }
    pub fn blocks(&self) -> u64 {
        self.cum.len() as u64 - 1
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
    fn block_count(&self, b: u64) -> u64 {
        self.cum[b as usize + 1] - self.cum[b as usize]
    }
}

/// Builds an index from bytes fed in order (used while writing files, so they don't need a second pass).
pub struct IndexBuilder {
    cum: Vec<u64>,
    in_block: u64,
    count: u64,
}

impl Default for IndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexBuilder {
    pub fn new() -> Self {
        IndexBuilder { cum: vec![0], in_block: 0, count: 0 }
    }
    pub fn push(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let room = (BLOCK - self.in_block) as usize;
            let n = room.min(data.len());
            self.count += bytecount::count(&data[..n], b'\n') as u64;
            self.in_block += n as u64;
            data = &data[n..];
            if self.in_block == BLOCK {
                self.cum.push(self.count);
                self.in_block = 0;
            }
        }
    }
    pub fn newlines(&self) -> u64 {
        self.count
    }
    pub fn finish(mut self) -> NlIndex {
        if self.in_block > 0 {
            self.cum.push(self.count);
        }
        NlIndex { cum: self.cum, complete: true }
    }
}

struct BlockCache {
    map: HashMap<u64, (Arc<[u8]>, u64)>,
    tick: u64,
}

enum Store {
    Mem(Box<[u8]>),
    /// `opened`: the file's stamp and identity through `file` when this source was made. `bulk`: a second handle on
    /// it, opened when first needed (None if it couldn't be: `file` does).
    File {
        file: File,
        bulk: OnceLock<Option<File>>,
        cache: Mutex<BlockCache>,
        path: PathBuf,
        kind: SourceKind,
        opened: Option<(Stamp, FileId)>,
    },
}

/// Where a source's bytes are (and so what keeping a document for a later run takes, see session.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// Memory: typed or pasted text, small files.
    Memory,
    /// A file of the user's, read where it is.
    File,
    /// A self-deleting temp file: a big file converted from UTF-16 or ANSI, the result of a big transform.
    Temp,
    /// A file in Slate's session folder (unsaved text kept from an earlier run): Slate's own, nobody writes it.
    Session,
}

pub struct Source {
    store: Store,
    len: u64,
    index: RwLock<NlIndex>,
    read_errors: AtomicU64,
    /// Hashes of sample blocks of the bytes the index was built from (see `reuse_index_from`, `changed_in_place`).
    fingerprint: Mutex<Option<Fingerprint>>,
    /// The samples `reuse_index_from` checked, for the part of the index it took over.
    reused: Mutex<Vec<(u64, u64)>>,
    /// A stamp the file was last found to only have grown (or got new times) at: no need to check it again.
    verified: Mutex<Option<Stamp>>,
    /// Why building the index stopped: the file couldn't be read, or changed while it was read.
    index_error: Mutex<Option<String>>,
}

/// What a file looks like through a handle: its size, and its last-write and change times. Another program
/// writing to the file changes it; a new file put in its place (by renaming over it) doesn't, as the handle stays
/// on the old one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub written: i64,
    pub changed: i64,
}

/// Which file a handle is on: the volume's serial number and the file's index on that volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    pub volume: u32,
    pub index: u64,
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

/// What a file held when it was indexed: hashes of sample blocks (complete ones spread over it) and of the
/// incomplete block at its end, if any (where a file that only grows is written next).
#[derive(Clone, Debug)]
struct Fingerprint {
    len: u64,
    samples: Vec<(u64, u64)>,
    tail: Option<u64>,
}

/// Which complete blocks of a file of `len` bytes a fingerprint samples.
fn sample_blocks(len: u64) -> Vec<u64> {
    let full = len / BLOCK;
    if full == 0 {
        return Vec::new();
    }
    let mut v: Vec<u64> = (0..SAMPLES).map(|k| full * k / SAMPLES).collect();
    v.push(full - 1);
    v.sort_unstable();
    v.dedup();
    v
}

/// Keeps at most `max` (at least 2) of the sorted samples `v`, evenly spread, the first and last among them.
fn thin(v: &mut Vec<(u64, u64)>, max: usize) {
    let n = v.len();
    if n > max {
        *v = (0..max).map(|k| v[k * (n - 1) / (max - 1)]).collect();
    }
}

/// A 64-bit hash that stays the same across runs and versions (the session keeps fingerprints for later runs),
/// for telling whether bytes changed: not meant to withstand someone trying to fool it.
pub fn stable_hash(data: &[u8]) -> u64 {
    const M: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = (data.len() as u64).wrapping_mul(M) ^ 0x243F_6A88_85A3_08D3;
    let (words, rest) = data.as_chunks::<8>();
    for w in words {
        h = (h ^ u64::from_le_bytes(*w)).wrapping_mul(M).rotate_left(29);
    }
    if !rest.is_empty() {
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        h = (h ^ u64::from_le_bytes(last)).wrapping_mul(M).rotate_left(29);
    }
    // (splitmix64's finish)
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

fn hash_bytes(data: &[u8]) -> u64 {
    stable_hash(data)
}

/// What the session keeps of a file a document is read from, to tell in a later run whether the file still holds
/// the same bytes (see `Source::same_as`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub path: PathBuf,
    /// How much of the file the document reads (the file's size when it was opened).
    pub len: u64,
    /// The file's stamp and identity then.
    pub size: u64,
    pub written: i64,
    pub changed: i64,
    pub volume: u32,
    pub index: u64,
    /// `stable_hash`es of sample blocks of `[0, len)` and of its incomplete last block.
    pub samples: Vec<(u64, u64)>,
    pub tail: Option<u64>,
}

/// A second handle on the file `file` is open on (that very file, even if another one took its name since).
fn reopen(file: &File) -> Option<File> {
    let h = HANDLE(file.as_raw_handle());
    let h = unsafe { ReOpenFile(h, GENERIC_READ.0, FILE_SHARE_MODE(SHARE_ALL), FILE_FLAGS_AND_ATTRIBUTES(0)) }.ok()?;
    Some(unsafe { File::from_raw_handle(h.0) })
}

/// Reads all of `buf` at `off` (an error if the file ends first). Counts nothing.
fn read_exact_at(file: &File, off: u64, buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match file.seek_read(&mut buf[done..], off + done as u64) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `read_exact_at`, trying again a few times (`RETRY_MS`) while it fails; gives up early when cancelled.
fn read_retrying(file: &File, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> io::Result<()> {
    let mut r = read_exact_at(file, off, buf);
    for ms in RETRY_MS {
        if r.is_ok() {
            break;
        }
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            if cancel.load(Ordering::Relaxed) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        r = read_exact_at(file, off, buf);
    }
    r
}

/// Why a read failed, in a few words for the user.
fn read_failure(e: &io::Error) -> String {
    match (e.kind(), e.raw_os_error()) {
        (io::ErrorKind::UnexpectedEof, _) => "it got shorter while Slate was reading it".into(),
        (_, Some(33)) => "another program has locked part of it".into(),
        (_, Some(53 | 59 | 64 | 121 | 1231)) => "the network drive stopped answering".into(),
        _ => {
            let s = e.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].trim_end_matches('.').to_string(),
                None => s,
            }
        }
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static TEMP_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Where temp files go (default: the system temp folder).
pub fn set_temp_dir(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    *TEMP_DIR.write().unwrap() = Some(dir);
}

/// A file in the temp folder that deletes itself when its handle closes (also after a crash).
pub fn create_temp_file() -> io::Result<(File, PathBuf)> {
    let dir = TEMP_DIR.read().unwrap().clone().unwrap_or_else(std::env::temp_dir);
    for _ in 0..100 {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("slate-{}-{}.tmp", std::process::id(), n));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .attributes(FILE_ATTRIBUTE_TEMPORARY | FILE_ATTRIBUTE_HIDDEN)
            .open(&path)
        {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("could not create a temporary file"))
}

impl Source {
    pub fn from_vec(data: Vec<u8>) -> Source {
        let mut b = IndexBuilder::new();
        b.push(&data);
        let len = data.len() as u64;
        Source::new(Store::Mem(data.into_boxed_slice()), len, b.finish())
    }

    fn new(store: Store, len: u64, index: NlIndex) -> Source {
        Source {
            store,
            len,
            index: RwLock::new(index),
            read_errors: AtomicU64::new(0),
            fingerprint: Mutex::new(None),
            reused: Mutex::new(Vec::new()),
            verified: Mutex::new(None),
            index_error: Mutex::new(None),
        }
    }

    /// Opens a file for on-demand reading without blocking anyone else from writing, renaming or deleting it.
    /// The newline index starts empty; call `build_index` (usually on a background thread).
    pub fn open_file(path: &Path) -> io::Result<Source> {
        let file = OpenOptions::new().read(true).share_mode(SHARE_ALL).open(path)?;
        let len = file.metadata()?.len();
        Ok(Source::from_file(file, len, path.to_path_buf(), false, None))
    }

    /// Wraps an already written file (a temp file, or a file just saved) whose index is known.
    pub fn from_file(file: File, len: u64, path: PathBuf, temp: bool, index: Option<NlIndex>) -> Source {
        let kind = if temp { SourceKind::Temp } else { SourceKind::File };
        Source::wrap(file, len, path, kind, index)
    }

    /// Opens the first `len` bytes of a file of Slate's session folder (text kept from an earlier run; nobody else
    /// writes it, and Slate only ever adds to its end). The index starts empty, as for `open_file`.
    pub fn open_session_file(path: &Path, len: u64) -> io::Result<Source> {
        let file = OpenOptions::new().read(true).share_mode(SHARE_ALL).open(path)?;
        if file.metadata()?.len() < len {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the session file is shorter than its list says"));
        }
        Ok(Source::wrap(file, len, path.to_path_buf(), SourceKind::Session, None))
    }

    fn wrap(file: File, len: u64, path: PathBuf, kind: SourceKind, index: Option<NlIndex>) -> Source {
        let indexed = index.as_ref().is_some_and(|i| i.complete);
        let opened = stamp_of(&file);
        let cache = Mutex::new(BlockCache { map: HashMap::new(), tick: 0 });
        let store = Store::File { file, bulk: OnceLock::new(), cache, path, kind, opened };
        let s = Source::new(store, len, index.unwrap_or_else(NlIndex::empty));
        if indexed && kind == SourceKind::File {
            *s.fingerprint.lock().unwrap() = s.take_fingerprint(s.len);
        }
        s
    }

    pub fn kind(&self) -> SourceKind {
        match &self.store {
            Store::Mem(_) => SourceKind::Memory,
            Store::File { kind, .. } => *kind,
        }
    }

    /// What a later run needs to tell whether this file still holds what the document reads from it (a file of the
    /// user's whose index is done; None otherwise).
    pub fn identity(&self) -> Option<Identity> {
        let Store::File { path, kind: SourceKind::File, opened: Some((stamp, id)), .. } = &self.store else {
            return None;
        };
        let fp = self.fingerprint.lock().unwrap().clone()?;
        Some(Identity {
            path: path.clone(),
            len: fp.len,
            size: stamp.size,
            written: stamp.written,
            changed: stamp.changed,
            volume: id.volume,
            index: id.index,
            samples: fp.samples,
            tail: fp.tail,
        })
    }

    /// Whether this source (the file `id` names, opened again in a later run) is still that file with the same
    /// bytes in `[0, id.len)`: the same file, not shorter, and either not written to since (same size and last-write
    /// time) or only grown (a log), and the same at the sampled places. Err says why not, for the user.
    pub fn same_as(&self, id: &Identity) -> Result<(), String> {
        let Store::File { opened: Some((stamp, fid)), .. } = &self.store else {
            return Err("Slate couldn't tell which file it is".into());
        };
        if id.index != 0 && fid.index != 0 && (fid.volume, fid.index) != (id.volume, id.index) {
            return Err("another file was put in its place".into());
        }
        if stamp.size < id.len {
            return Err("it is shorter now".into());
        }
        // Same size but written to: rewritten in place (that a few samples can't rule out). Grown: a log, if the
        // samples (and what was its last, incomplete block) say so.
        if stamp.size == id.size && stamp.written != id.written {
            return Err("another program changed it".into());
        }
        let fp = Fingerprint { len: id.len, samples: id.samples.clone(), tail: id.tail };
        if !self.matches(&fp) {
            return Err("another program changed it".into());
        }
        Ok(())
    }

    /// The fingerprint of the first `len` bytes, read straight from the file (None if a read failed).
    fn take_fingerprint(&self, len: u64) -> Option<Fingerprint> {
        let mut samples = Vec::new();
        for b in sample_blocks(len) {
            samples.push((b, self.hash_range(b * BLOCK, (b + 1) * BLOCK)?));
        }
        let t = len / BLOCK * BLOCK;
        let tail = if t < len { Some(self.hash_range(t, len)?) } else { None };
        Some(Fingerprint { len, samples, tail })
    }

    /// Hash of `[a, b)` read straight from the file (None if the file doesn't have all of it or the read fails).
    fn hash_range(&self, a: u64, b: u64) -> Option<u64> {
        if !self.is_file() || b > self.len {
            return None;
        }
        let mut buf = vec![0u8; (b - a) as usize];
        read_exact_at(self.bulk(), a, &mut buf).ok()?;
        Some(hash_bytes(&buf))
    }

    /// Whether this source's file still has what `fp` saw, at its samples and its incomplete last block.
    fn matches(&self, fp: &Fingerprint) -> bool {
        if self.len < fp.len {
            return false;
        }
        if fp.samples.iter().any(|&(b, h)| self.hash_range(b * BLOCK, (b + 1) * BLOCK) != Some(h)) {
            return false;
        }
        fp.tail.is_none_or(|h| self.hash_range(fp.len / BLOCK * BLOCK, fp.len) == Some(h))
    }

    /// Whether another program has written into the file this source reads since it was opened, so it no longer
    /// holds what was read from it. A file that only grew (a log being written) or only got new times hasn't:
    /// when the size or times differ, the fingerprint's samples are read again to tell. A file put in place of
    /// ours (renamed over it) doesn't count either: the handle still reads the old one. Memory and temp files are
    /// never changed by others.
    pub fn changed_in_place(&self) -> bool {
        let Store::File { kind: SourceKind::File, opened: Some((then, _)), .. } = &self.store else {
            return false;
        };
        // (asked on another thread every few seconds: not through the window's handle)
        let Some((now, _)) = stamp_of(self.bulk()) else { return false };
        if now == *then || *self.verified.lock().unwrap() == Some(now) {
            return false;
        }
        if now.size < self.len {
            return true;
        }
        let fp = self.fingerprint.lock().unwrap().clone();
        let same = fp.is_some_and(|fp| self.matches(&fp));
        if same {
            *self.verified.lock().unwrap() = Some(now);
        }
        !same
    }

    /// Why the index couldn't be finished (see `build_index`), if it couldn't.
    pub fn index_error(&self) -> Option<String> {
        self.index_error.lock().unwrap().clone()
    }

    fn stop_index(&self, why: String) {
        *self.index_error.lock().unwrap() = Some(why);
        self.read_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_file(&self) -> bool {
        matches!(self.store, Store::File { .. })
    }
    /// The file of the user's this source reads from (not for temp files and Slate's session files).
    pub fn file_path(&self) -> Option<&Path> {
        match &self.store {
            Store::File { path, kind: SourceKind::File, .. } => Some(path),
            _ => None,
        }
    }
    pub fn read_errors(&self) -> u64 {
        self.read_errors.load(Ordering::Relaxed)
    }
    pub fn index_complete(&self) -> bool {
        self.index.read().unwrap().complete
    }
    /// Bytes covered by the newline index so far.
    pub fn indexed_bytes(&self) -> u64 {
        let idx = self.index.read().unwrap();
        if idx.complete { self.len } else { (idx.blocks() * BLOCK).min(self.len) }
    }
    /// Zero-copy access for in-memory sources.
    pub fn mem(&self) -> Option<&[u8]> {
        match &self.store {
            Store::Mem(d) => Some(d),
            _ => None,
        }
    }

    /// The handle for reading a lot at once (see the module docs): the second one, or `file` if there's none.
    fn bulk(&self) -> &File {
        let Store::File { file, bulk, .. } = &self.store else { unreachable!() };
        bulk.get_or_init(|| reopen(file)).as_ref().unwrap_or(file)
    }

    /// Reads `buf.len()` bytes at `off`; missing bytes become zeros and count as a read error. Returns whether
    /// everything was read.
    fn file_read(&self, file: &File, off: u64, buf: &mut [u8]) -> bool {
        let mut done = 0;
        while done < buf.len() {
            match file.seek_read(&mut buf[done..], off + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if done < buf.len() {
            buf[done..].fill(0);
            self.read_errors.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn block(&self, b: u64) -> Arc<[u8]> {
        let Store::File { file, cache, .. } = &self.store else { unreachable!() };
        {
            let mut c = cache.lock().unwrap();
            c.tick += 1;
            let t = c.tick;
            if let Some(e) = c.map.get_mut(&b) {
                e.1 = t;
                return e.0.clone();
            }
        }
        let start = b * BLOCK;
        let n = (self.len - start).min(BLOCK) as usize;
        let mut data = vec![0u8; n];
        let complete = self.file_read(file, start, &mut data);
        let data: Arc<[u8]> = data.into();
        if !complete {
            // Never cache a failed read: the next read must try again (and count the error again, so saving
            // refuses to write the zeros).
            return data;
        }
        let mut c = cache.lock().unwrap();
        if c.map.len() >= CACHE_BLOCKS {
            if let Some((&old, _)) = c.map.iter().min_by_key(|(_, v)| v.1) {
                c.map.remove(&old);
            }
        }
        let t = c.tick;
        c.map.insert(b, (data.clone(), t));
        data
    }

    /// Calls `f` with consecutive slices covering `[start, end)`; stops early (returning false) if `f` does.
    /// Small ranges go through the block cache; big sequential ranges are read directly.
    pub fn chunks(&self, start: u64, end: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        let end = end.min(self.len);
        if start >= end {
            return true;
        }
        match &self.store {
            Store::Mem(d) => f(&d[start as usize..end as usize]),
            Store::File { .. } => {
                if end - start <= 4 * BLOCK {
                    let mut pos = start;
                    while pos < end {
                        let b = pos / BLOCK;
                        let blk = self.block(b);
                        let s = (pos - b * BLOCK) as usize;
                        let e = ((end - b * BLOCK) as usize).min(blk.len());
                        if !f(&blk[s..e]) {
                            return false;
                        }
                        pos = b * BLOCK + e as u64;
                    }
                    true
                } else {
                    let mut buf = vec![0u8; (1 << 20).min((end - start) as usize)];
                    let mut pos = start;
                    while pos < end {
                        let n = ((end - pos) as usize).min(buf.len());
                        self.file_read(self.bulk(), pos, &mut buf[..n]);
                        if !f(&buf[..n]) {
                            return false;
                        }
                        pos += n as u64;
                    }
                    true
                }
            }
        }
    }

    /// Appends `[start, end)` to `out`.
    pub fn read_into(&self, start: u64, end: u64, out: &mut Vec<u8>) {
        let end = end.min(self.len);
        if start >= end {
            return;
        }
        match &self.store {
            Store::Mem(d) => out.extend_from_slice(&d[start as usize..end as usize]),
            Store::File { .. } => {
                if end - start <= 4 * BLOCK {
                    self.chunks(start, end, &mut |c| {
                        out.extend_from_slice(c);
                        true
                    });
                } else {
                    let old = out.len();
                    out.resize(old + (end - start) as usize, 0);
                    self.file_read(self.bulk(), start, &mut out[old..]);
                }
            }
        }
    }

    pub fn byte_at(&self, pos: u64) -> u8 {
        match &self.store {
            Store::Mem(d) => d[pos as usize],
            Store::File { .. } => {
                let b = pos / BLOCK;
                self.block(b)[(pos - b * BLOCK) as usize]
            }
        }
    }

    fn count_direct(&self, a: u64, b: u64) -> u64 {
        let mut n = 0u64;
        self.chunks(a, b, &mut |c| {
            n += bytecount::count(c, b'\n') as u64;
            true
        });
        n
    }

    /// Number of `\n` in `[a, b)`.
    pub fn count_nl(&self, a: u64, b: u64) -> u64 {
        let b = b.min(self.len);
        if a >= b {
            return 0;
        }
        let (lo, hi, mid) = {
            let idx = self.index.read().unwrap();
            let lo = a.div_ceil(BLOCK);
            let hi = (b / BLOCK).min(idx.blocks());
            if hi <= lo {
                (0, 0, None)
            } else {
                (lo, hi, Some(idx.cum[hi as usize] - idx.cum[lo as usize]))
            }
        };
        match mid {
            Some(m) => self.count_direct(a, lo * BLOCK) + m + self.count_direct(hi * BLOCK, b),
            None => self.count_direct(a, b),
        }
    }

    /// Position of the `k`-th (1-based) `\n` in `[a, b)`.
    pub fn nth_nl(&self, a: u64, b: u64, k: u64) -> Option<u64> {
        let b = b.min(self.len);
        if k == 0 || a >= b {
            return None;
        }
        let mut k = k;
        let head_end = b.min(a.div_ceil(BLOCK) * BLOCK);
        if let Some(p) = self.scan_nth(a, head_end, &mut k) {
            return Some(p);
        }
        let mut pos = head_end;
        if pos >= b {
            return None;
        }
        enum Jump {
            /// The newline is the `k`-th one in block `blk`.
            Into { blk: u64, k: u64 },
            /// It is past the indexed blocks; skip `skipped` newlines up to block `to`.
            Past { to: u64, skipped: u64 },
            None,
        }
        let jump = {
            let idx = self.index.read().unwrap();
            let lo = pos / BLOCK;
            let hi = (b / BLOCK).min(idx.blocks());
            if hi <= lo {
                Jump::None
            } else {
                let total = idx.cum[hi as usize] - idx.cum[lo as usize];
                if total >= k {
                    let target = idx.cum[lo as usize] + k;
                    // First t in (lo, hi] with cum[t] >= target: the newline is in block t-1.
                    let after = &idx.cum[lo as usize + 1..=hi as usize];
                    let blk = lo + after.partition_point(|&c| c < target) as u64;
                    Jump::Into { blk, k: target - idx.cum[blk as usize] }
                } else {
                    Jump::Past { to: hi, skipped: total }
                }
            }
        };
        match jump {
            Jump::Into { blk, k: mut kk } => self.scan_nth(blk * BLOCK, ((blk + 1) * BLOCK).min(b), &mut kk),
            Jump::Past { to, skipped } => {
                k -= skipped;
                pos = to * BLOCK;
                self.scan_nth(pos, b, &mut k)
            }
            Jump::None => self.scan_nth(pos, b, &mut k),
        }
    }

    fn scan_nth(&self, a: u64, b: u64, k: &mut u64) -> Option<u64> {
        let mut found = None;
        let mut pos = a;
        self.chunks(a, b, &mut |c| {
            let n = bytecount::count(c, b'\n') as u64;
            if n < *k {
                *k -= n;
                pos += c.len() as u64;
                return true;
            }
            for (i, p) in memchr::memchr_iter(b'\n', c).enumerate() {
                if i as u64 + 1 == *k {
                    found = Some(pos + p as u64);
                    break;
                }
            }
            false
        });
        found
    }

    /// First `\n` in `[a, b)`.
    pub fn find_nl_fwd(&self, a: u64, b: u64) -> Option<u64> {
        let b = b.min(self.len);
        let mut pos = a;
        while pos < b {
            let blk = pos / BLOCK;
            let blk_end = ((blk + 1) * BLOCK).min(b);
            // Skip indexed runs without newlines in one go.
            {
                let idx = self.index.read().unwrap();
                if blk < idx.blocks() && idx.block_count(blk) == 0 {
                    let mut nb = blk + 1;
                    while nb < idx.blocks() && nb * BLOCK < b && idx.block_count(nb) == 0 {
                        nb += 1;
                    }
                    pos = nb * BLOCK;
                    continue;
                }
            }
            let mut found = None;
            let mut p = pos;
            self.chunks(pos, blk_end, &mut |c| {
                if let Some(i) = memchr::memchr(b'\n', c) {
                    found = Some(p + i as u64);
                    return false;
                }
                p += c.len() as u64;
                true
            });
            if found.is_some() {
                return found;
            }
            pos = blk_end;
        }
        None
    }

    /// Last `\n` in `[a, b)`.
    pub fn find_nl_back(&self, a: u64, b: u64) -> Option<u64> {
        let b = b.min(self.len);
        let mut pos = b;
        while pos > a {
            let blk = (pos - 1) / BLOCK;
            let start = a.max(blk * BLOCK);
            {
                let idx = self.index.read().unwrap();
                if blk < idx.blocks() && idx.block_count(blk) == 0 {
                    let mut nb = blk;
                    while nb > 0 && nb * BLOCK > a && idx.block_count(nb - 1) == 0 {
                        nb -= 1;
                    }
                    pos = a.max(nb * BLOCK);
                    continue;
                }
            }
            let mut buf = Vec::with_capacity((pos - start) as usize);
            self.read_into(start, pos, &mut buf);
            if let Some(i) = memchr::memrchr(b'\n', &buf) {
                return Some(start + i as u64);
            }
            pos = start;
        }
        None
    }

    /// For a file that only grew (a log): starts this source's index with `old`'s complete blocks, so only the new
    /// tail gets indexed. Only when this file still has, at the sampled places and in the incomplete block that
    /// ended it, exactly what `old` had when it was indexed; otherwise the file was rewritten and gets indexed
    /// from scratch. Returns whether it reused.
    pub fn reuse_index_from(&self, old: &Source) -> bool {
        let Some(fp) = old.fingerprint.lock().unwrap().clone() else { return false };
        if fp.samples.is_empty() || fp.len != old.len || !self.matches(&fp) {
            return false;
        }
        let o = old.index.read().unwrap();
        let full = (old.len / BLOCK).min(o.blocks()) as usize;
        let mut idx = self.index.write().unwrap();
        if idx.blocks() == 0 && !idx.complete && full > 0 {
            idx.cum = o.cum[..=full].to_vec();
            *self.reused.lock().unwrap() = fp.samples;
            return true;
        }
        false
    }

    /// Indexes the whole source, reading it sequentially. Meant for a background thread; the index becomes usable
    /// prefix by prefix while this runs. Returns false if cancelled, or if it had to stop (`index_error` says why):
    /// a read that kept failing (a guessed count would let line commands run over text they shouldn't), or the
    /// file was written over while it was read. Either way the index is never marked complete.
    pub fn build_index(&self, cancel: &AtomicBool, progress: &AtomicU64) -> bool {
        self.build_index_at(cancel, progress, 0)
    }

    /// `build_index`, counting progress from `base` (several files read one after the other).
    pub fn build_index_at(&self, cancel: &AtomicBool, progress: &AtomicU64, base: u64) -> bool {
        let Store::File { opened, .. } = &self.store else {
            return true;
        };
        let file = self.bulk();
        if self.index.read().unwrap().complete {
            return true;
        }
        const CHUNK: usize = 4 << 20;
        let mut buf = vec![0u8; CHUNK];
        let (mut pos, mut count) = {
            let idx = self.index.read().unwrap();
            (idx.blocks() * BLOCK, *idx.cum.last().unwrap())
        };
        // The fingerprint comes from the very bytes counted here (for a part reused from an earlier version of the
        // file: from the samples `reuse_index_from` checked there).
        let first = pos / BLOCK;
        let wanted = sample_blocks(self.len);
        let mut samples: Vec<(u64, u64)> =
            self.reused.lock().unwrap().iter().copied().filter(|s| s.0 < first).collect();
        if samples.is_empty() {
            for &b in wanted.iter().filter(|&&b| b < first) {
                match self.hash_range(b * BLOCK, (b + 1) * BLOCK) {
                    Some(h) => samples.push((b, h)),
                    None => {
                        self.stop_index("part of it couldn't be read".into());
                        return false;
                    }
                }
            }
        }
        let mut tail = None;
        let mut pending: Vec<u64> = Vec::new();
        while pos < self.len {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let n = ((self.len - pos) as usize).min(CHUNK);
            if let Err(e) = read_retrying(file, pos, &mut buf[..n], cancel) {
                if !cancel.load(Ordering::Relaxed) {
                    self.stop_index(read_failure(&e));
                }
                return false;
            }
            for (k, block) in buf[..n].chunks(BLOCK as usize).enumerate() {
                let b = pos / BLOCK + k as u64;
                count += bytecount::count(block, b'\n') as u64;
                pending.push(count);
                if block.len() < BLOCK as usize {
                    tail = Some(hash_bytes(block));
                } else if wanted.binary_search(&b).is_ok() {
                    samples.push((b, hash_bytes(block)));
                }
            }
            pos += n as u64;
            progress.store(base + pos, Ordering::Relaxed);
            let mut idx = self.index.write().unwrap();
            idx.cum.append(&mut pending);
        }
        samples.sort_unstable();
        samples.dedup_by_key(|s| s.0);
        thin(&mut samples, 2 * SAMPLES as usize);
        let fp = Fingerprint { len: self.len, samples, tail };
        // Written to while it was read? Fine if it only grew (a log); otherwise what was counted isn't what it holds.
        let now = stamp_of(file).map(|s| s.0);
        if now.is_some() && now != opened.map(|s| s.0) && !self.matches(&fp) {
            self.stop_index("it changed while Slate was reading it".into());
            return false;
        }
        *self.fingerprint.lock().unwrap() = Some(fp);
        self.index.write().unwrap().complete = true;
        progress.store(base + self.len, Ordering::Relaxed);
        true
    }

    /// Writes `[start, end)` to `w`, feeding the bytes to `idx` too.
    pub fn copy_to(&self, start: u64, end: u64, w: &mut dyn Write, idx: &mut IndexBuilder) -> io::Result<()> {
        let mut err = None;
        self.chunks(start, end, &mut |c| {
            idx.push(c);
            if let Err(e) = w.write_all(c) {
                err = Some(e);
                return false;
            }
            true
        });
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                if x % 37 == 0 { b'\n' } else { b'a' + (x % 26) as u8 }
            })
            .collect()
    }

    fn file_source(data: &[u8], indexed: bool) -> Source {
        let (mut f, path) = create_temp_file().unwrap();
        f.write_all(data).unwrap();
        let s = Source::from_file(f, data.len() as u64, path, true, None);
        if indexed {
            assert!(s.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        }
        s
    }

    #[test]
    fn index_queries_match_naive() {
        let data = sample(700_000, 7);
        let nls: Vec<u64> = data.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i as u64).collect();
        for src in [Source::from_vec(data.clone()), file_source(&data, true), file_source(&data, false)] {
            let ranges = [(0u64, 700_000u64), (1, 65_536), (65_535, 131_073), (100_000, 650_001), (5, 6), (699_990, 700_000)];
            for &(a, b) in &ranges {
                let inside: Vec<u64> = nls.iter().copied().filter(|&p| p >= a && p < b).collect();
                assert_eq!(src.count_nl(a, b), inside.len() as u64, "count {a}..{b}");
                for k in [1usize, 2, inside.len() / 2, inside.len()] {
                    if k == 0 {
                        continue;
                    }
                    assert_eq!(src.nth_nl(a, b, k as u64), inside.get(k - 1).copied(), "nth {k} in {a}..{b}");
                }
                assert_eq!(src.nth_nl(a, b, inside.len() as u64 + 1), None);
                assert_eq!(src.find_nl_fwd(a, b), inside.first().copied(), "fwd {a}..{b}");
                assert_eq!(src.find_nl_back(a, b), inside.last().copied(), "back {a}..{b}");
            }
        }
    }

    #[test]
    fn long_line_skips_empty_blocks() {
        let mut data = vec![b'x'; 1_000_000];
        data[10] = b'\n';
        data[999_000] = b'\n';
        let src = file_source(&data, true);
        assert_eq!(src.find_nl_fwd(11, 1_000_000), Some(999_000));
        assert_eq!(src.find_nl_back(0, 999_000), Some(10));
        assert_eq!(src.count_nl(0, 1_000_000), 2);
        assert_eq!(src.nth_nl(0, 1_000_000, 2), Some(999_000));
    }

    #[test]
    fn short_file_reads_as_zeros_and_counts_errors() {
        let data = sample(1000, 3);
        let (mut f, path) = create_temp_file().unwrap();
        f.write_all(&data).unwrap();
        let s = Source::from_file(f, 2000, path, true, None);
        let mut out = Vec::new();
        s.read_into(900, 1100, &mut out);
        assert_eq!(&out[..100], &data[900..]);
        assert!(out[100..].iter().all(|&b| b == 0));
        assert!(s.read_errors() > 0);
    }

    #[test]
    fn index_is_reused_only_when_the_file_just_grew() {
        let dir = std::env::temp_dir().join(format!("slate-test-{}-reuse", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        let lines: Vec<u8> = (0..60_000u32).flat_map(|i| format!("entry {i:06} ok\n").into_bytes()).collect();
        std::fs::write(&path, &lines).unwrap();
        let old = Source::open_file(&path).unwrap();
        assert!(old.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        // appended: reused, and the counts are right
        let mut grown = lines.clone();
        grown.extend_from_slice(b"one more\nand another\n");
        std::fs::write(&path, &grown).unwrap();
        let new = Source::open_file(&path).unwrap();
        assert!(new.reuse_index_from(&old));
        assert!(new.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        assert_eq!(new.count_nl(0, new.len()), 60_002);
        // rewritten in place with the same length but different line breaks: indexed from scratch
        let mut rewritten = grown.clone();
        for (i, b) in rewritten.iter_mut().enumerate() {
            if i % 7 == 3 && *b == b' ' {
                *b = b'\n';
            }
        }
        let expect = bytecount::count(&rewritten, b'\n') as u64;
        let f = OpenOptions::new().write(true).share_mode(0x7).open(&path).unwrap();
        f.seek_write(&rewritten, 0).unwrap();
        drop(f);
        let newer = Source::open_file(&path).unwrap();
        assert!(!newer.reuse_index_from(&new));
        assert!(newer.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        assert_eq!(newer.count_nl(0, newer.len()), expect);
        drop((old, new, newer));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_reads_are_not_cached() {
        let data = sample(200_000, 9);
        let (mut f, path) = create_temp_file().unwrap();
        f.write_all(&data).unwrap();
        let s = Source::from_file(f, data.len() as u64, path, true, None);
        let Store::File { file, .. } = &s.store else { unreachable!() };
        file.set_len(100_000).unwrap();
        let mut out = Vec::new();
        s.read_into(150_000, 150_100, &mut out);
        let after_first = s.read_errors();
        assert!(after_first > 0);
        out.clear();
        s.read_into(150_000, 150_100, &mut out);
        assert!(s.read_errors() > after_first, "a second read of missing bytes must count again");
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slate-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes `data` to `dir/name` and opens it as an indexed source.
    fn indexed(dir: &Path, name: &str, data: &[u8]) -> (PathBuf, Source) {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        let s = Source::open_file(&path).unwrap();
        assert!(s.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        (path, s)
    }

    fn write_at(path: &Path, off: u64, data: &[u8]) {
        let f = OpenOptions::new().write(true).share_mode(0x7).open(path).unwrap();
        f.seek_write(data, off).unwrap();
    }

    #[test]
    fn changed_in_place_tells_a_rewrite_from_a_file_that_grew() {
        let dir = test_dir("src-inplace");
        let data = sample(700_000, 5);
        let (_, same) = indexed(&dir, "same.txt", &data);
        assert!(!same.changed_in_place());
        // a log being written: only grew
        let (p, grew) = indexed(&dir, "grew.txt", &data);
        write_at(&p, data.len() as u64, b"more lines\n");
        assert!(!grew.changed_in_place());
        // only new times (opened for writing, nothing changed)
        let (p, touched) = indexed(&dir, "touched.txt", &data);
        let f = OpenOptions::new().write(true).share_mode(0x7).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)).unwrap();
        drop(f);
        assert!(!touched.changed_in_place());
        // rewritten in place, same length, other content (truncated and written, like most programs save)
        let (p, rewritten) = indexed(&dir, "rewritten.txt", &data);
        std::fs::write(&p, sample(700_000, 6)).unwrap();
        assert!(rewritten.changed_in_place());
        // the incomplete last block changed (and it grew): not just appended to
        let (p, end) = indexed(&dir, "end.txt", &data);
        write_at(&p, data.len() as u64 - 10, b"0123456789 and more\n");
        assert!(end.changed_in_place());
        // cut short
        let (p, cut) = indexed(&dir, "cut.txt", &data);
        OpenOptions::new().write(true).share_mode(0x7).open(&p).unwrap().set_len(1000).unwrap();
        assert!(cut.changed_in_place());
        drop((same, grew, touched, rewritten, end, cut));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_reads_are_tried_again_and_never_guessed() {
        let dir = test_dir("idxerr");
        let data = sample(300_000, 8);
        // unreadable for a moment (a network drive that dropped): tried again, and the counts are right
        let path = dir.join("blip.txt");
        std::fs::write(&path, &data).unwrap();
        let s = Source::open_file(&path).unwrap();
        OpenOptions::new().write(true).share_mode(0x7).open(&path).unwrap().set_len(1000).unwrap();
        let restore = {
            let (path, data) = (path.clone(), data.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                write_at(&path, 0, &data);
            })
        };
        assert!(s.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        restore.join().unwrap();
        assert_eq!(s.count_nl(0, s.len()), bytecount::count(&data, b'\n') as u64);
        assert_eq!(s.index_error(), None);
        // unreadable for good: stops, says why, and the index isn't complete (nothing is guessed)
        let path = dir.join("gone.txt");
        std::fs::write(&path, &data).unwrap();
        let s = Source::open_file(&path).unwrap();
        OpenOptions::new().write(true).share_mode(0x7).open(&path).unwrap().set_len(1000).unwrap();
        assert!(!s.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        assert!(!s.index_complete());
        assert!(s.index_error().is_some_and(|w| w.contains("shorter")), "{:?}", s.index_error());
        assert!(s.read_errors() > 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bulk_reads_have_their_own_handle_on_the_same_file() {
        let dir = test_dir("bulk");
        let data = sample(700_000, 11);
        let (path, s) = indexed(&dir, "big.txt", &data);
        // the index was read through a second handle, on the very same file
        let Store::File { file, bulk, .. } = &s.store else { unreachable!() };
        let second = bulk.get().and_then(Option::as_ref).expect("a second handle");
        assert_ne!(second.as_raw_handle(), file.as_raw_handle());
        assert_eq!(stamp_of(second).map(|s| s.1), stamp_of(file).map(|s| s.1));
        // another file put in its place: both still read the old one
        std::fs::write(dir.join("new.txt"), sample(700_000, 12)).unwrap();
        std::fs::rename(dir.join("new.txt"), &path).unwrap();
        let (mut big, mut small) = (Vec::new(), Vec::new());
        s.read_into(0, s.len(), &mut big);
        s.read_into(100_000, 100_100, &mut small);
        assert_eq!(big, data);
        assert_eq!(small, &data[100_000..100_100]);
        assert!(!s.changed_in_place());
        // deleted before a second handle was needed: still read (through the first one, if need be)
        let path = dir.join("gone.txt");
        std::fs::write(&path, &data).unwrap();
        let gone = Source::open_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut out = Vec::new();
        gone.read_into(0, gone.len(), &mut out);
        assert_eq!((out == data, gone.read_errors()), (true, 0));
        // a self-deleting temp file
        let temp = file_source(&data, true);
        let mut out = Vec::new();
        temp.read_into(0, temp.len(), &mut out);
        assert_eq!(out, data);
        let Store::File { bulk, .. } = &temp.store else { unreachable!() };
        assert!(bulk.get().is_some_and(Option::is_some));
        drop((s, gone, temp));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stable_hashes_stay_the_same() {
        // (kept in sessions: if this changed, every big document's file would look changed to the next version)
        assert_eq!(stable_hash(b""), 0xe9e0_033e_3bad_af36);
        assert_eq!(stable_hash(b"Slate"), 0x0d4e_8e8c_c4af_5df3);
        assert_eq!(stable_hash(&sample(70_000, 1)), 0x3726_030c_d0a4_959a);
        assert_ne!(stable_hash(b"Slate"), stable_hash(b"slate"));
    }

    #[test]
    fn index_is_not_reused_when_the_old_end_changed() {
        let dir = test_dir("reuse-end");
        let data = sample(700_000, 4);
        let (path, old) = indexed(&dir, "log.txt", &data);
        // the last (incomplete) block of what was indexed got other bytes, and more was added: rewritten
        write_at(&path, data.len() as u64 - 100, &[b'\n'; 300]);
        let new = Source::open_file(&path).unwrap();
        assert!(!new.reuse_index_from(&old));
        // a plain append is still reused
        let (path, old) = indexed(&dir, "log2.txt", &data);
        write_at(&path, data.len() as u64, b"appended\n");
        let new = Source::open_file(&path).unwrap();
        assert!(new.reuse_index_from(&old));
        assert!(new.build_index(&AtomicBool::new(false), &AtomicU64::new(0)));
        assert_eq!(new.count_nl(0, new.len()), bytecount::count(&data, b'\n') as u64 + 1);
        drop((old, new));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
