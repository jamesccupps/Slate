//! Screen readers (UI Automation): Narrator, NVDA and JAWS read the text, follow the caret and the selection, and
//! hear edits.
//!
//! The window answers WM_GETOBJECT for UiaRootObjectId with `Root`, which adds one child to the window's own element
//! (its title bar and frame, and the find bar's edit boxes, stay Windows'): the active tab's text (`Text`), a Document
//! named after the tab, with the Text pattern (`ITextProvider2`). Its ranges (`Range`) are two byte offsets into the
//! tab's document, kept in `RANGES` and moved by every edit (`follow`, from `View::sync`) as the scroll position is.
//! The units (`Unit`) are a character (as the caret moves: a letter with its accents, an emoji, `\r\n`), a word
//! (letters and digits, or punctuation, with the spaces after them; each line starts a new one), a line (a row on
//! screen, wrapped or not), a paragraph (a line of the document) and the document; UI Automation's format counts as a
//! word and its page as the document (the next bigger unit, as it asks).
//!
//! UIAutomationCore.dll is loaded when a client first asks for the window's element (`api`): a Slate no screen reader
//! looks at starts and runs as before. Events (`after`, after each message and each test command) are raised only
//! while clients listen: focus when the text (or another tab's) gets the keyboard, text selection changed when the
//! caret or the selection moved, text changed after an edit, the name when the tab's title changed.
//!
//! The providers hold no reference to the App: each call finds it on the UI thread (`UseComThreading` brings calls
//! there) and fails with UIA_E_ELEMENTNOTAVAILABLE while it is borrowed (a call that comes in while a dialog's loop
//! runs), when the tab or its document is gone, or on another thread; a panic is caught, never unwinding across COM.
//! A range can be a whole 800 MB file: what's read for a client stops at `TEXT_MAX`, and a move by many units after
//! `MOVE_TIME` (by characters or paragraphs, they're counted in bulk instead).

// (UI Automation's constants, matched by their names: `TextUnit_Line`, `UIA_NamePropertyId`…)
#![allow(non_upper_case_globals)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::panic::AssertUnwindSafe;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, E_FAIL, E_INVALIDARG, HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VT_I4, VT_R8, VT_UNKNOWN};
use windows::Win32::UI::Accessibility::*;
use windows::core::{BSTR, Error, HRESULT, IUnknown, IUnknownImpl, Interface, PCSTR, Result, VARIANT, implement, s, w};

use super::app::{App, Cell};
use super::editor::{Ctx, View};
use super::gfx::Rect;
use crate::core::document::{Change, Document, Sel};
use crate::core::search::{Matcher, Query};
use crate::core::text::{self, CharClass};

// ---- UIAutomationCore.dll, loaded when a client first asks ----

type ReturnProvider = unsafe extern "system" fn(HWND, WPARAM, LPARAM, *mut c_void) -> LRESULT;
type HostFromHwnd = unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT;
type RaiseEvent = unsafe extern "system" fn(*mut c_void, UIA_EVENT_ID) -> HRESULT;
type RaiseProperty =
    unsafe extern "system" fn(*mut c_void, UIA_PROPERTY_ID, MaybeUninit<VARIANT>, MaybeUninit<VARIANT>) -> HRESULT;
type Listening = unsafe extern "system" fn() -> BOOL;
type Reserved = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;

/// The functions of UIAutomationCore.dll Slate calls.
struct Api {
    return_provider: ReturnProvider,
    host_from_hwnd: HostFromHwnd,
    raise_event: RaiseEvent,
    raise_property: RaiseProperty,
    listening: Listening,
    not_supported: Reserved,
}

static API: OnceLock<Option<Api>> = OnceLock::new();

/// UIAutomationCore.dll's functions, loaded from System32 the first time they're needed (None if that fails).
fn api() -> Option<&'static Api> {
    API.get_or_init(|| unsafe {
        let dll = LoadLibraryExW(w!("UIAutomationCore.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        Some(Api {
            return_provider: export(dll, s!("UiaReturnRawElementProvider"))?,
            host_from_hwnd: export(dll, s!("UiaHostProviderFromHwnd"))?,
            raise_event: export(dll, s!("UiaRaiseAutomationEvent"))?,
            raise_property: export(dll, s!("UiaRaiseAutomationPropertyChangedEvent"))?,
            listening: export(dll, s!("UiaClientsAreListening"))?,
            not_supported: export(dll, s!("UiaGetReservedNotSupportedValue"))?,
        })
    })
    .as_ref()
}

/// The function `name` of `dll` as `F`, an `extern "system" fn` of its signature.
unsafe fn export<F: Copy>(dll: HMODULE, name: PCSTR) -> Option<F> {
    let f = unsafe { GetProcAddress(dll, name) }?;
    debug_assert_eq!(size_of::<F>(), size_of_val(&f));
    Some(unsafe { std::mem::transmute_copy(&f) })
}

/// The functions, if a client has asked for the window's element already (this loads nothing).
fn loaded() -> Option<&'static Api> {
    API.get().and_then(Option::as_ref)
}

/// Whether clients listen for events (never before one has asked for the window's element).
fn listening() -> bool {
    loaded().is_some_and(|a| unsafe { (a.listening)() }.as_bool())
}

// ---- the window ----

/// WM_GETOBJECT: the window's UI Automation element (None for the other objects: Windows answers those).
pub fn on_getobject(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    // (the object id is a DWORD, sign-extended or not)
    if lp.0 as i32 != UiaRootObjectId {
        return None;
    }
    let api = api()?;
    let root: IRawElementProviderSimple = Root { hwnd }.into();
    Some(unsafe { (api.return_provider)(hwnd, wp, lp, root.as_raw()) })
}

/// WM_DESTROY: clients let go of the window's providers.
pub fn on_destroy(hwnd: HWND) {
    if let Some(api) = loaded() {
        unsafe { (api.return_provider)(hwnd, WPARAM(0), LPARAM(0), std::ptr::null_mut()) };
    }
}

// ---- reaching the App ----

fn unavailable() -> Error {
    Error::from(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

/// No element (or range) to give: S_OK with a null pointer.
fn none<T>() -> Result<T> {
    Err(Error::empty())
}

/// Runs `f` on the App: on the UI thread, while nothing else has it borrowed. A panic comes back as E_FAIL.
fn with_app<R>(f: impl FnOnce(&mut App) -> Result<R>) -> Result<R> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let cell = super::app_cell().ok_or_else(unavailable)?;
        let mut a = cell.try_borrow_mut().map_err(|_| unavailable())?;
        f(&mut a)
    }))
    .unwrap_or_else(|_| Err(E_FAIL.into()))
}

/// The index of tab `id`, its view brought up to date with its document's edits (and the ranges with them).
fn tab_index(a: &mut App, id: u64) -> Result<usize> {
    let i = a.tabs.iter().position(|t| t.id == id).ok_or_else(unavailable)?;
    let t = &mut a.tabs[i];
    t.view.sync(&mut t.doc);
    Ok(i)
}

/// Runs `f` with tab `i`'s view and its context (the text area as it is now).
fn with_view<R>(a: &mut App, i: usize, f: impl FnOnce(&mut View, &Ctx) -> R) -> R {
    let t = &a.tabs[i];
    let geom = View::geometry(&t.doc, &a.style, a.r_edit, t.filter.as_ref().map(|f| f.map.last_source() + 1));
    let t = &mut a.tabs[i];
    let line_map = t.filter.as_ref().map(|f| &*f.map);
    let cx = Ctx { doc: &t.doc, g: &a.g, style: &a.style, theme: &a.theme, lang: t.lang, geom, line_map };
    f(&mut t.view, &cx)
}

/// Runs `f` with tab `i`'s units (its rows as lines).
fn with_units<R>(a: &mut App, i: usize, f: impl FnOnce(&mut ViewUnits) -> R) -> R {
    with_view(a, i, |view, cx| f(&mut ViewUnits { view, cx }))
}

/// Whether the text of the window has the keyboard (not the find bar's boxes, nor another window).
fn text_focused(a: &App) -> bool {
    super::win::focus() == a.hwnd
}

/// A rectangle of the client area (DIPs) on the screen, in pixels.
fn on_screen(hwnd: HWND, dpi: u32, r: Rect) -> UiaRect {
    let k = dpi as f64 / 96.0;
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut p);
    }
    UiaRect { left: p.x as f64 + r.x as f64 * k, top: p.y as f64 + r.y as f64 * k, width: r.w as f64 * k, height: r.h as f64 * k }
}

/// A point on the screen (pixels), in the client area's DIPs.
fn in_client(a: &App, x: f64, y: f64) -> (f32, f32) {
    let mut p = POINT { x: x.round() as i32, y: y.round() as i32 };
    unsafe {
        let _ = ScreenToClient(a.hwnd, &mut p);
    }
    (a.px_to_dip(p.x), a.px_to_dip(p.y))
}

fn array_of_i32(v: &[i32]) -> *mut SAFEARRAY {
    unsafe {
        let sa = SafeArrayCreateVector(VT_I4, 0, v.len() as u32);
        for (i, x) in v.iter().enumerate() {
            let _ = SafeArrayPutElement(sa, &(i as i32), x as *const i32 as *const c_void);
        }
        sa
    }
}

fn array_of_f64(v: &[f64]) -> *mut SAFEARRAY {
    unsafe {
        let sa = SafeArrayCreateVector(VT_R8, 0, v.len() as u32);
        for (i, x) in v.iter().enumerate() {
            let _ = SafeArrayPutElement(sa, &(i as i32), x as *const f64 as *const c_void);
        }
        sa
    }
}

fn array_of_ranges(v: &[ITextRangeProvider]) -> *mut SAFEARRAY {
    unsafe {
        let sa = SafeArrayCreateVector(VT_UNKNOWN, 0, v.len() as u32);
        for (i, r) in v.iter().enumerate() {
            // (VT_UNKNOWN takes the pointer itself, and adds a reference)
            let _ = SafeArrayPutElement(sa, &(i as i32), r.as_raw());
        }
        sa
    }
}

const OPTIONS: ProviderOptions = ProviderOptions(ProviderOptions_ServerSideProvider.0 | ProviderOptions_UseComThreading.0);

// ---- units ----

/// What ranges move and expand by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Char,
    Word,
    /// A row on screen.
    Line,
    /// A line of the document.
    Para,
    Doc,
}

impl Unit {
    /// UI Automation's unit: its format counts as a word and its page as the document (the next bigger unit there is).
    fn of(u: TextUnit) -> Unit {
        match u {
            TextUnit_Character => Unit::Char,
            TextUnit_Format | TextUnit_Word => Unit::Word,
            TextUnit_Line => Unit::Line,
            TextUnit_Paragraph => Unit::Para,
            _ => Unit::Doc,
        }
    }
}

/// Where the units of a text are: the boundaries ranges move by.
pub trait Units {
    /// Where the text ends.
    fn end(&self) -> u64;
    /// The next boundary of `u` after `pos` (the end of the text is one), None at the end.
    fn next(&mut self, u: Unit, pos: u64) -> Option<u64>;
    /// The boundary of `u` before `pos` (the start is one), None at the start.
    fn prev(&mut self, u: Unit, pos: u64) -> Option<u64>;
    /// Whether a unit starts at `pos`: a boundary before the end (or the end, where it starts an empty last line).
    fn starts(&mut self, u: Unit, pos: u64) -> bool;
    /// Moves `pos` by `count` units at once where that's quicker than one at a time (None where it isn't).
    fn skip(&mut self, _u: Unit, _pos: u64, _count: i32) -> Option<(u64, i32)> {
        None
    }
}

/// A move by many units (rows of a huge file, say) stops after this long, as far as it got.
const MOVE_TIME: Duration = Duration::from_millis(250);

/// Where the unit `pos` is in starts.
fn unit_start(t: &mut impl Units, u: Unit, pos: u64) -> u64 {
    if t.starts(u, pos) { pos } else { t.prev(u, pos).unwrap_or(0) }
}

/// The unit at `pos` (ExpandToEnclosingUnit): the one starting there, or around it. At the end of the text a
/// character or word is empty (there's nothing more to read), a line or paragraph the last one.
pub fn enclosing(t: &mut impl Units, u: Unit, pos: u64) -> (u64, u64) {
    let len = t.end();
    let pos = pos.min(len);
    if u == Unit::Doc {
        return (0, len);
    }
    if pos == len && matches!(u, Unit::Char | Unit::Word) {
        return (len, len);
    }
    let a = unit_start(t, u, pos);
    (a, t.next(u, a).unwrap_or(len))
}

/// Moves `pos` by `count` boundaries of `u`, fewer at either end of the text: where it ends up, and by how many.
pub fn move_by(t: &mut impl Units, u: Unit, pos: u64, count: i32) -> (u64, i32) {
    if let Some(r) = t.skip(u, pos, count) {
        return r;
    }
    let started = Instant::now();
    let (mut p, mut n) = (pos, 0i32);
    while n != count {
        let next = if count > 0 { t.next(u, p) } else { t.prev(u, p) };
        let Some(q) = next else { break };
        p = q;
        n += count.signum();
        if n % 1024 == 0 && started.elapsed() > MOVE_TIME {
            break;
        }
    }
    (p, n)
}

/// ITextRangeProvider::Move: an empty range moves as a caret does; another becomes the unit `count` units after (or
/// before) the one it starts in, never the empty end of the text. A range that can't move stays as it is.
pub fn move_range(t: &mut impl Units, u: Unit, (a, b): (u64, u64), count: i32) -> ((u64, u64), i32) {
    if count == 0 {
        return ((a, b), 0);
    }
    if a == b {
        let (p, n) = move_by(t, u, a, count);
        return ((p, p), n);
    }
    let start = unit_start(t, u, a);
    let (mut p, mut n) = move_by(t, u, start, count);
    if n > 0 && !t.starts(u, p) {
        // (the end of the text, after a last line that isn't empty: that's no unit to stand on)
        p = t.prev(u, p).unwrap_or(start);
        n -= 1;
    }
    if n == 0 {
        return ((a, b), 0);
    }
    let len = t.end();
    ((p, t.next(u, p).unwrap_or(len)), n)
}

/// ITextRangeProvider::MoveEndpointByUnit: the start (or `end`) moves by `count` boundaries; past the other end, it
/// takes that along.
pub fn move_endpoint(t: &mut impl Units, u: Unit, (a, b): (u64, u64), end: bool, count: i32) -> ((u64, u64), i32) {
    let (p, n) = move_by(t, u, if end { b } else { a }, count);
    (if end { (a.min(p), p) } else { (p, b.max(p)) }, n)
}

/// How far a word is looked for: a "word" longer than this (a line of base64, say) is cut there.
const WORD_MAX: u64 = 1 << 16;

/// The character people see at `i` (a letter with its accents, `\r\n`): its length, its first character, and
/// whether it ends a line.
fn cluster(buf: &[u8], i: usize) -> (usize, char, bool) {
    let b = &buf[i..];
    let n = match b {
        [b'\r', b'\n', ..] => 2,
        [b'\r' | b'\n', ..] => 1,
        _ => text::cluster_len_at(b).max(1),
    }
    .min(b.len());
    (n, text::decode_char(b).0, b[n - 1] == b'\n')
}

/// Whether a word starts at a character `c` after one of class `before` (None at the start of the text, or of a line:
/// `nl`): where letters and digits, or punctuation, start after something else. Spaces and line breaks belong to the
/// word before them.
fn word_boundary(before: Option<CharClass>, nl: bool, c: char) -> bool {
    let blank = |k: CharClass| matches!(k, CharClass::Space | CharClass::Newline);
    match before {
        None => true,
        _ if nl => true,
        Some(b) => {
            let k = text::char_class(c);
            !blank(k) && (blank(b) || b != k)
        }
    }
}

/// The first word boundary after `pos` (`pos < len`), or the end of the text.
fn next_word(doc: &Document, pos: u64) -> u64 {
    let len = doc.len();
    let limit = (pos + WORD_MAX).min(len);
    // (a little more, so the last character read is whole)
    let buf = doc.read(pos, (limit + 16).min(len));
    let lim = (limit - pos) as usize;
    let (n, c, nl) = cluster(&buf, 0);
    let (mut class, mut nl, mut i) = (text::char_class(c), nl, n);
    while i < lim {
        let (n, c, ends_nl) = cluster(&buf, i);
        if word_boundary(Some(class), nl, c) {
            return pos + i as u64;
        }
        (class, nl, i) = (text::char_class(c), ends_nl, i + n);
    }
    pos + i.min(buf.len()) as u64
}

/// The last word boundary before `pos` (`pos > 0`).
fn prev_word(doc: &Document, pos: u64) -> u64 {
    // Words are found from a known start: the start of the line the character before `pos` is on, or (a line longer
    // than `WORD_MAX`) a character start that far back.
    let from = pos.saturating_sub(WORD_MAX);
    let buf = doc.read(from, pos);
    let start = match memchr::memrchr(b'\n', &buf[..buf.len() - 1]) {
        Some(j) => j + 1,
        None if from == 0 => 0,
        None => buf.iter().position(|&b| b & 0xC0 != 0x80).unwrap_or(buf.len()),
    };
    if start >= buf.len() {
        // (nothing but bytes that aren't text)
        return crate::edit::prev_cluster(doc, pos).min(pos - 1);
    }
    let (mut best, mut before, mut nl, mut i) = (start, None, false, start);
    while i < buf.len() {
        let (n, c, ends_nl) = cluster(&buf, i);
        if word_boundary(before, nl, c) {
            best = i;
        }
        (before, nl, i) = (Some(text::char_class(c)), ends_nl, i + n);
    }
    from + best as u64
}

/// Whether a word starts at `pos` (`pos < len`).
fn word_starts(doc: &Document, pos: u64) -> bool {
    if pos == 0 {
        return true;
    }
    // (the character before, from its start; the one at `pos`)
    let p = crate::edit::prev_cluster(doc, pos).min(pos - 1);
    let buf = doc.read(p, (pos + 8).min(doc.len()));
    let k = (pos - p) as usize;
    let before = text::char_class(text::decode_char(&buf).0);
    word_boundary(Some(before), buf[k - 1] == b'\n', text::decode_char(&buf[k..]).0)
}

/// Moves `pos` by `count` characters at once (many of them), counted as UTF-8 characters with `\r\n` as one (an
/// accent counts on its own here, where the caret steps over it with its letter).
fn skip_chars(doc: &Document, pos: u64, count: i32) -> (u64, i32) {
    let want = count.unsigned_abs() as u64;
    let len = doc.len();
    let crlf = |c: &[u8]| memchr::memmem::find_iter(c, b"\r\n").count() as u64;
    let mut n = 0u64;
    if count > 0 {
        let (mut at, mut cr, mut found) = (pos, false, None);
        doc.chunks(pos, len, &mut |c: &[u8]| {
            let starts = bytecount::num_chars(c) as u64 - crlf(c) - u64::from(cr && c.first() == Some(&b'\n'));
            if n + starts <= want {
                n += starts;
            } else {
                for (i, &b) in c.iter().enumerate() {
                    if b & 0xC0 != 0x80 && !(b == b'\n' && (if i == 0 { cr } else { c[i - 1] == b'\r' })) {
                        if n == want {
                            found = Some(at + i as u64);
                            return false;
                        }
                        n += 1;
                    }
                }
            }
            cr = c.last() == Some(&b'\r');
            at += c.len() as u64;
            true
        });
        (found.unwrap_or(len), n as i32)
    } else {
        let mut e = pos.min(len);
        while e > 0 {
            let s = e.saturating_sub(1 << 20);
            // (and the byte before, for a `\r\n` across)
            let buf = doc.read(s.saturating_sub(1), e);
            let off = (s - s.saturating_sub(1)) as usize;
            let start = |i: usize| buf[i] & 0xC0 != 0x80 && !(buf[i] == b'\n' && i > 0 && buf[i - 1] == b'\r');
            let starts = bytecount::num_chars(&buf[off..]) as u64 - crlf(&buf[off.saturating_sub(1)..]);
            if n + starts < want {
                n += starts;
                e = s;
                continue;
            }
            for i in (off..buf.len()).rev() {
                if start(i) {
                    n += 1;
                    if n == want {
                        return (s + (i - off) as u64, -(n as i32));
                    }
                }
            }
            e = s;
        }
        (0, -(n as i32))
    }
}

/// Moves `pos` by `count` paragraphs at once, by the line index (None while the document's lines are being counted).
fn skip_paras(doc: &Document, pos: u64, count: i32) -> Option<(u64, i32)> {
    let len = doc.len();
    let lines = doc.line_count()?;
    let l = doc.line_of(pos)?;
    let ls = doc.line_start(l)?;
    if count > 0 {
        // the starts of the lines after `l`, then the end (unless the last line is empty: its start is the end)
        let after = (lines - 1 - l) + u64::from(pos < len && doc.line_start(lines - 1)? < len);
        let k = (count as u64).min(after);
        let p = if k == 0 { pos } else if l + k < lines { doc.line_start(l + k)? } else { len };
        Some((p, k as i32))
    } else {
        // the start of `pos`'s line (if it's past it), then those of the lines before
        let before = l + u64::from(ls < pos);
        let k = (count.unsigned_abs() as u64).min(before);
        let p = if k == 0 { pos } else { doc.line_start(if ls < pos { l + 1 - k } else { l - k })? };
        Some((p, -(k as i32)))
    }
}

/// A document's units; a line is a line of it (`ViewUnits` makes it a row on screen).
pub struct DocUnits<'a> {
    pub doc: &'a Document,
}

impl Units for DocUnits<'_> {
    fn end(&self) -> u64 {
        self.doc.len()
    }

    fn next(&mut self, u: Unit, pos: u64) -> Option<u64> {
        let len = self.doc.len();
        if pos >= len {
            return None;
        }
        Some(match u {
            Unit::Char => crate::edit::next_cluster(self.doc, pos).clamp(pos + 1, len),
            Unit::Word => next_word(self.doc, pos),
            Unit::Line | Unit::Para => self.doc.next_newline(pos).map_or(len, |p| p + 1),
            Unit::Doc => len,
        })
    }

    fn prev(&mut self, u: Unit, pos: u64) -> Option<u64> {
        let pos = pos.min(self.doc.len());
        if pos == 0 {
            return None;
        }
        Some(match u {
            Unit::Char => crate::edit::prev_cluster(self.doc, pos).min(pos - 1),
            Unit::Word => prev_word(self.doc, pos),
            Unit::Line | Unit::Para => match self.doc.line_start_of(pos) {
                ls if ls < pos => ls,
                _ => self.doc.line_start_of(pos - 1),
            },
            Unit::Doc => 0,
        })
    }

    fn starts(&mut self, u: Unit, pos: u64) -> bool {
        let len = self.doc.len();
        match u {
            Unit::Char => pos < len,
            Unit::Word => pos < len && word_starts(self.doc, pos),
            Unit::Line | Unit::Para => pos == 0 || (pos <= len && self.doc.byte_at(pos - 1) == Some(b'\n')),
            Unit::Doc => pos == 0,
        }
    }

    fn skip(&mut self, u: Unit, pos: u64, count: i32) -> Option<(u64, i32)> {
        match u {
            Unit::Char if count.unsigned_abs() > 4096 => Some(skip_chars(self.doc, pos, count)),
            Unit::Para if count.unsigned_abs() > 64 => skip_paras(self.doc, pos, count),
            _ => None,
        }
    }
}

/// A tab's units, its lines the rows of its view (wrapped or not, as they show).
pub struct ViewUnits<'a, 'b> {
    view: &'a mut View,
    cx: &'a Ctx<'b>,
}

impl ViewUnits<'_, '_> {
    fn doc(&self) -> DocUnits<'_> {
        DocUnits { doc: self.cx.doc }
    }
}

impl Units for ViewUnits<'_, '_> {
    fn end(&self) -> u64 {
        self.cx.doc.len()
    }

    fn next(&mut self, u: Unit, pos: u64) -> Option<u64> {
        if u != Unit::Line {
            return self.doc().next(u, pos);
        }
        let len = self.end();
        if pos >= len {
            return None;
        }
        let rs = self.view.row_start(self.cx, pos);
        Some(self.view.next_row(self.cx, rs).unwrap_or(len).clamp(pos + 1, len))
    }

    fn prev(&mut self, u: Unit, pos: u64) -> Option<u64> {
        if u != Unit::Line {
            return self.doc().prev(u, pos);
        }
        let pos = pos.min(self.end());
        if pos == 0 {
            return None;
        }
        let rs = self.view.row_start(self.cx, pos);
        if rs < pos { Some(rs) } else { self.view.prev_row(self.cx, rs).map(|p| p.min(pos - 1)) }
    }

    fn starts(&mut self, u: Unit, pos: u64) -> bool {
        if u != Unit::Line {
            return self.doc().starts(u, pos);
        }
        pos <= self.end() && self.view.row_start(self.cx, pos) == pos
    }

    fn skip(&mut self, u: Unit, pos: u64, count: i32) -> Option<(u64, i32)> {
        if u == Unit::Line { None } else { self.doc().skip(u, pos, count) }
    }
}

/// The most text read for a client at once (UTF-16 units): about 2 MB, where a range can be a whole huge file.
const TEXT_MAX: usize = 1 << 20;

/// The text of `[a, b)` as UTF-16, at most `max` units (`-1`: up to `TEXT_MAX`); bytes that aren't UTF-8 read as
/// U+FFFD.
fn text_of(doc: &Document, a: u64, b: u64, max: i32) -> Vec<u16> {
    let max = if max < 0 { TEXT_MAX } else { (max as usize).min(TEXT_MAX) };
    // (a unit takes at most 3 bytes: a 4-byte character is two)
    let end = b.min(a.saturating_add(3 * max as u64 + 3));
    let mut bytes = doc.read(a, end);
    // (not a character cut in two)
    if end < b
        && let Some(i) = bytes.iter().rposition(|&c| c & 0xC0 != 0x80)
        && i + text::char_len_at(&bytes[i..]).max(1) > bytes.len()
    {
        bytes.truncate(i);
    }
    let mut out = Vec::new();
    for c in String::from_utf8_lossy(&bytes).chars() {
        let mut u = [0u16; 2];
        let u = c.encode_utf16(&mut u);
        if out.len() + u.len() > max {
            break;
        }
        out.extend_from_slice(u);
    }
    out
}

// ---- the window's element ----

/// The window's element as Slate adds to it: the active tab's text as its child (Windows gives the rest).
#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot)]
struct Root {
    hwnd: HWND,
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(OPTIONS)
    }

    fn GetPatternProvider(&self, _: UIA_PATTERN_ID) -> Result<IUnknown> {
        none()
    }

    fn GetPropertyValue(&self, _: UIA_PROPERTY_ID) -> Result<VARIANT> {
        // (the window's own: its title as its name, a window)
        Ok(VARIANT::default())
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        let api = api().ok_or_else(unavailable)?;
        let mut p = std::ptr::null_mut();
        unsafe {
            (api.host_from_hwnd)(self.hwnd, &mut p).ok()?;
            if p.is_null() {
                return none();
            }
            // (the reference it gave is ours)
            Ok(IRawElementProviderSimple::from_raw(p))
        }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, dir: NavigateDirection) -> Result<IRawElementProviderFragment> {
        if dir != NavigateDirection_FirstChild && dir != NavigateDirection_LastChild {
            return none();
        }
        let tab = with_app(|a| Ok(a.tab().id))?;
        Ok(Text { hwnd: self.hwnd, tab }.into())
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // (the window's own)
        Ok(std::ptr::null_mut())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let tab = with_app(|a| {
            let (cx, cy) = in_client(a, x, y);
            Ok(a.r_edit.contains(cx, cy).then(|| a.tab().id))
        })?;
        match tab {
            Some(tab) => Ok(Text { hwnd: self.hwnd, tab }.into()),
            None => none(),
        }
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        match with_app(|a| Ok(text_focused(a).then(|| a.tab().id)))? {
            Some(tab) => Ok(Text { hwnd: self.hwnd, tab }.into()),
            None => none(),
        }
    }
}

// ---- a tab's text ----

/// The text of tab `tab`: a Document named after the tab, with the Text pattern.
#[implement(IRawElementProviderSimple, IRawElementProviderFragment, ITextProvider2)]
struct Text {
    hwnd: HWND,
    tab: u64,
}

impl Text {
    fn range(&self, doc: u64, a: u64, b: u64) -> ITextRangeProvider {
        new_range(self.hwnd, self.tab, doc, a, b)
    }
}

impl IRawElementProviderSimple_Impl for Text_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(OPTIONS)
    }

    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        if id == UIA_TextPatternId || id == UIA_TextPattern2Id { Ok(self.to_interface()) } else { none() }
    }

    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        with_app(|a| {
            let i = tab_index(a, self.tab)?;
            let shown = i == a.active;
            Ok(match id {
                UIA_ControlTypePropertyId => VARIANT::from(UIA_DocumentControlTypeId.0),
                UIA_NamePropertyId => VARIANT::from(a.tabs[i].title().as_str()),
                UIA_AutomationIdPropertyId => VARIANT::from("Text"),
                UIA_IsKeyboardFocusablePropertyId | UIA_IsEnabledPropertyId | UIA_IsTextPatternAvailablePropertyId => {
                    VARIANT::from(true)
                }
                UIA_HasKeyboardFocusPropertyId => VARIANT::from(shown && text_focused(a)),
                UIA_IsOffscreenPropertyId => VARIANT::from(!shown),
                _ => VARIANT::default(),
            })
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        none()
    }
}

impl IRawElementProviderFragment_Impl for Text_Impl {
    fn Navigate(&self, dir: NavigateDirection) -> Result<IRawElementProviderFragment> {
        if dir == NavigateDirection_Parent { Ok(Root { hwnd: self.hwnd }.into()) } else { none() }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(array_of_i32(&[UiaAppendRuntimeId as i32, 1, self.tab as i32]))
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        with_app(|a| {
            let i = tab_index(a, self.tab)?;
            Ok(if i == a.active { on_screen(a.hwnd, a.dpi, a.r_edit) } else { UiaRect::default() })
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        let hwnd = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            if i == a.active { Ok(a.hwnd) } else { Err(unavailable()) }
        })?;
        // (outside the App's borrow: the window's WM_SETFOCUS takes it)
        super::win::set_focus(hwnd);
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root { hwnd: self.hwnd }.into())
    }
}

impl ITextProvider_Impl for Text_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let (doc, sel) = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            Ok((a.tabs[i].doc.id(), a.tabs[i].view.sel))
        })?;
        Ok(array_of_ranges(&[self.range(doc, sel.start(), sel.end())]))
    }

    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let shown = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            if i != a.active {
                return Ok(None);
            }
            let rows = with_view(a, i, |v, cx| {
                v.layout_rows(cx);
                v.rows.first().zip(v.rows.last()).map(|(f, l)| (f.start, l.end))
            });
            Ok(rows.map(|r| (a.tabs[i].doc.id(), r)))
        })?;
        Ok(array_of_ranges(&shown.map(|(doc, (s, e))| self.range(doc, s, e)).into_iter().collect::<Vec<_>>()))
    }

    fn RangeFromChild(&self, _: Option<&IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        // (no elements inside the text)
        Err(E_INVALIDARG.into())
    }

    fn RangeFromPoint(&self, p: &UiaPoint) -> Result<ITextRangeProvider> {
        let (doc, pos) = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            if i != a.active {
                return Err(unavailable());
            }
            let (x, y) = in_client(a, p.x, p.y);
            // (the nearest place in the text for a point outside it)
            let r = a.r_edit;
            let (x, y) = (x.clamp(r.x, r.right() - 1.0), y.clamp(r.y, r.bottom() - 1.0));
            let pos = with_view(a, i, |v, cx| v.pos_at(cx, x, y).0);
            Ok((a.tabs[i].doc.id(), pos))
        })?;
        Ok(self.range(doc, pos, pos))
    }

    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let (doc, len) = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            Ok((a.tabs[i].doc.id(), a.tabs[i].doc.len()))
        })?;
        Ok(self.range(doc, 0, len))
    }

    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}

impl ITextProvider2_Impl for Text_Impl {
    fn RangeFromAnnotation(&self, _: Option<&IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(E_INVALIDARG.into())
    }

    fn GetCaretRange(&self, active: *mut BOOL) -> Result<ITextRangeProvider> {
        let (doc, caret, focused) = with_app(|a| {
            let i = tab_index(a, self.tab)?;
            Ok((a.tabs[i].doc.id(), a.tabs[i].view.sel.caret, i == a.active && text_focused(a)))
        })?;
        if !active.is_null() {
            unsafe { *active = focused.into() };
        }
        Ok(self.range(doc, caret, caret))
    }
}

// ---- text ranges ----

thread_local! {
    /// The text ranges clients hold, by id: (document id, start, end).
    static RANGES: RefCell<BTreeMap<u64, (u64, u64, u64)>> = const { RefCell::new(BTreeMap::new()) };
    static NEXT_RANGE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// At most this many ranges are kept; past that (a client that never lets go of its own), the oldest half goes.
const RANGES_MAX: usize = 100_000;

fn new_range(hwnd: HWND, tab: u64, doc: u64, a: u64, b: u64) -> ITextRangeProvider {
    let id = NEXT_RANGE.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    RANGES.with(|r| {
        let mut r = r.borrow_mut();
        if r.len() >= RANGES_MAX {
            let old: Vec<u64> = r.keys().take(RANGES_MAX / 2).copied().collect();
            for k in old {
                r.remove(&k);
            }
        }
        r.insert(id, (doc, a.min(b), a.max(b)));
    });
    Range { hwnd, tab, id }.into()
}

/// Moves the ranges of document `doc` with its edits (from `View::sync`).
pub fn follow(doc: u64, changes: &[Change]) {
    if changes.is_empty() {
        return;
    }
    let _ = RANGES.try_with(|r| {
        let Ok(mut r) = r.try_borrow_mut() else { return };
        for v in r.values_mut().filter(|v| v.0 == doc) {
            for c in changes {
                (v.1, v.2) = (c.map(v.1), c.map(v.2));
            }
        }
    });
}

/// A range of a tab's text: its ends are in `RANGES`.
#[implement(ITextRangeProvider)]
struct Range {
    hwnd: HWND,
    tab: u64,
    id: u64,
}

impl Drop for Range {
    fn drop(&mut self) {
        let _ = RANGES.try_with(|r| r.try_borrow_mut().map(|mut r| r.remove(&self.id)));
    }
}

impl Range {
    /// The range's tab (its index, its view synced) and ends; unavailable once its document is gone (a reload).
    fn place(&self, a: &mut App) -> Result<(usize, u64, u64)> {
        let i = tab_index(a, self.tab)?;
        let (doc, s, e) = RANGES.with(|r| r.borrow().get(&self.id).copied()).ok_or_else(unavailable)?;
        if a.tabs[i].doc.id() != doc {
            return Err(unavailable());
        }
        let len = a.tabs[i].doc.len();
        Ok((i, s.min(len), e.min(len)))
    }

    fn set(&self, (s, e): (u64, u64)) {
        RANGES.with(|r| {
            if let Some(v) = r.borrow_mut().get_mut(&self.id) {
                (v.1, v.2) = (s.min(e), s.max(e));
            }
        });
    }

    /// Another range of the same text, as a client hands it back.
    fn other<'r>(&self, r: Option<&'r ITextRangeProvider>) -> Result<&'r Range> {
        let o: &Range = r.ok_or(E_INVALIDARG)?.cast_object_ref::<Range>().map_err(|_| E_INVALIDARG)?;
        if o.tab == self.tab { Ok(o) } else { Err(E_INVALIDARG.into()) }
    }
}

fn is_end(e: TextPatternRangeEndpoint) -> bool {
    e == TextPatternRangeEndpoint_End
}

impl ITextRangeProvider_Impl for Range_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        let (doc, s, e) = RANGES.with(|r| r.borrow().get(&self.id).copied()).ok_or_else(unavailable)?;
        Ok(new_range(self.hwnd, self.tab, doc, s, e))
    }

    fn Compare(&self, range: Option<&ITextRangeProvider>) -> Result<BOOL> {
        let o = self.other(range)?;
        with_app(|a| {
            let (_, s, e) = self.place(a)?;
            let (_, os, oe) = o.place(a)?;
            Ok((s == os && e == oe).into())
        })
    }

    fn CompareEndpoints(
        &self,
        endpoint: TextPatternRangeEndpoint,
        target: Option<&ITextRangeProvider>,
        target_end: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let o = self.other(target)?;
        with_app(|a| {
            let (_, s, e) = self.place(a)?;
            let (_, os, oe) = o.place(a)?;
            let (p, q) = (if is_end(endpoint) { e } else { s }, if is_end(target_end) { oe } else { os });
            Ok(p.cmp(&q) as i32)
        })
    }

    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        with_app(|a| {
            let (i, s, _) = self.place(a)?;
            self.set(with_units(a, i, |t| enclosing(t, Unit::of(unit), s)));
            Ok(())
        })
    }

    fn FindAttribute(&self, _: UIA_TEXTATTRIBUTE_ID, _: &VARIANT, _: BOOL) -> Result<ITextRangeProvider> {
        // (one font, one color: no runs of attributes to look for)
        none()
    }

    fn FindText(&self, text: &BSTR, backward: BOOL, ignore_case: BOOL) -> Result<ITextRangeProvider> {
        let q = Query { text: text.to_string(), match_case: !ignore_case.as_bool(), whole_word: false, regex: false };
        if q.text.is_empty() {
            return Err(E_INVALIDARG.into());
        }
        let m = Matcher::new(&q).map_err(|_| Error::from(E_INVALIDARG))?;
        let hit = with_app(|a| {
            let (i, s, e) = self.place(a)?;
            let doc = &a.tabs[i].doc;
            let hit = if backward.as_bool() { m.find_back(doc, s, e, None) } else { m.find_fwd(doc, s, e, None) };
            Ok(hit.filter(|h| h.1 <= e).map(|(hs, he)| (doc.id(), hs, he)))
        })?;
        match hit {
            Some((doc, s, e)) => Ok(new_range(self.hwnd, self.tab, doc, s, e)),
            None => none(),
        }
    }

    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        let v = with_app(|a| {
            let (i, _, _) = self.place(a)?;
            let t = &a.tabs[i];
            Ok(match id {
                UIA_IsReadOnlyAttributeId => Some(VARIANT::from(t.read_only || t.filter.is_some())),
                UIA_FontNameAttributeId if !a.settings.font.is_empty() => Some(VARIANT::from(a.settings.font.as_str())),
                UIA_FontSizeAttributeId => Some(VARIANT::from((a.settings.font_size * a.settings.zoom) as f64)),
                UIA_IsHiddenAttributeId => Some(VARIANT::from(false)),
                _ => None,
            })
        })?;
        match v {
            Some(v) => Ok(v),
            None => {
                let api = api().ok_or_else(unavailable)?;
                let mut p = std::ptr::null_mut();
                unsafe {
                    (api.not_supported)(&mut p).ok()?;
                    // (a reference UI Automation keeps: this one is borrowed, not owned)
                    Ok(VARIANT::from(IUnknown::from_raw_borrowed(&p).ok_or(E_FAIL)?.clone()))
                }
            }
        }
    }

    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let rects = with_app(|a| {
            let (i, s, e) = self.place(a)?;
            if i != a.active {
                return Ok(Vec::new());
            }
            let (hwnd, dpi) = (a.hwnd, a.dpi);
            let rects = with_view(a, i, |v, cx| {
                v.layout_rows(cx);
                v.screen_rects(cx, s, e)
            });
            Ok(rects.into_iter().map(|r| on_screen(hwnd, dpi, r)).collect::<Vec<_>>())
        })?;
        Ok(array_of_f64(&rects.iter().flat_map(|r| [r.left, r.top, r.width, r.height]).collect::<Vec<_>>()))
    }

    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        Ok(Text { hwnd: self.hwnd, tab: self.tab }.into())
    }

    fn GetText(&self, max: i32) -> Result<BSTR> {
        let text = with_app(|a| {
            let (i, s, e) = self.place(a)?;
            Ok(text_of(&a.tabs[i].doc, s, e, max))
        })?;
        BSTR::from_wide(&text)
    }

    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        with_app(|a| {
            let (i, s, e) = self.place(a)?;
            let (r, n) = with_units(a, i, |t| move_range(t, Unit::of(unit), (s, e), count));
            self.set(r);
            Ok(n)
        })
    }

    fn MoveEndpointByUnit(&self, endpoint: TextPatternRangeEndpoint, unit: TextUnit, count: i32) -> Result<i32> {
        with_app(|a| {
            let (i, s, e) = self.place(a)?;
            let (r, n) = with_units(a, i, |t| move_endpoint(t, Unit::of(unit), (s, e), is_end(endpoint), count));
            self.set(r);
            Ok(n)
        })
    }

    fn MoveEndpointByRange(
        &self,
        endpoint: TextPatternRangeEndpoint,
        target: Option<&ITextRangeProvider>,
        target_end: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let o = self.other(target)?;
        with_app(|a| {
            let (_, s, e) = self.place(a)?;
            let (_, os, oe) = o.place(a)?;
            let p = if is_end(target_end) { oe } else { os };
            self.set(if is_end(endpoint) { (s.min(p), p) } else { (p, e.max(p)) });
            Ok(())
        })
    }

    fn Select(&self) -> Result<()> {
        with_app(|a| {
            let (i, s, e) = self.place(a)?;
            let v = &mut a.tabs[i].view;
            v.sel = Sel::new(s, e);
            (v.want_x, v.upstream) = (None, false);
            if i == a.active {
                with_view(a, i, |v, cx| v.reveal(cx, e, false));
                a.wake_caret();
                a.invalidate();
            }
            Ok(())
        })
    }

    fn AddToSelection(&self) -> Result<()> {
        // (one selection)
        Err(Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32)))
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        Err(Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32)))
    }

    fn ScrollIntoView(&self, top: BOOL) -> Result<()> {
        with_app(|a| {
            let (i, s, _) = self.place(a)?;
            if i == a.active {
                with_view(a, i, |v, cx| if top.as_bool() { v.scroll_to_offset(cx, s) } else { v.reveal(cx, s, false) });
                a.invalidate();
            }
            Ok(())
        })
    }

    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        Ok(array_of_ranges(&[]))
    }
}

// ---- events ----

/// What clients last heard: the active tab, its document and version, its selection, whether its text had the
/// keyboard, and its name.
#[derive(Clone, PartialEq)]
struct Told {
    tab: u64,
    doc: u64,
    version: u64,
    sel: Sel,
    focused: bool,
    name: String,
}

enum Event {
    Focus,
    Selection,
    Text,
    Name(String, String),
}

thread_local! {
    static TOLD: RefCell<Option<Told>> = const { RefCell::new(None) };
    /// Test mode: the events raised since `print:uia-events` last listed them.
    static RAISED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// After each message (and each test command): tells listening clients what changed since the last time. While none
/// listen nothing is kept, so one starting later begins from what it reads itself.
pub fn after(cell: &Cell) {
    let testing = super::win::testing();
    if !testing && !listening() {
        TOLD.with(|t| t.borrow_mut().take());
        return;
    }
    let Ok(a) = cell.try_borrow() else { return };
    let Some(t) = a.tabs.get(a.active) else { return };
    let now = Told {
        tab: t.id,
        doc: t.doc.id(),
        version: t.doc.version,
        sel: t.view.sel,
        focused: text_focused(&a),
        name: t.title(),
    };
    let hwnd = a.hwnd;
    drop(a);
    let Some(was) = TOLD.with(|t| t.replace(Some(now.clone()))) else { return };
    if was == now {
        return;
    }
    let mut events = Vec::new();
    if now.focused && (!was.focused || now.tab != was.tab) {
        // (another element has the keyboard: clients read it from the start)
        events.push(Event::Focus);
    } else if now.tab == was.tab {
        if now.name != was.name {
            events.push(Event::Name(was.name, now.name.clone()));
        }
        let new_text = now.doc != was.doc || now.version != was.version;
        if new_text {
            events.push(Event::Text);
        }
        if new_text || now.sel != was.sel {
            events.push(Event::Selection);
        }
    }
    raise(hwnd, now.tab, &events);
}

/// Raises `events` for tab `tab`'s text (outside the App's borrow: UI Automation may ask the element about itself).
fn raise(hwnd: HWND, tab: u64, events: &[Event]) {
    if events.is_empty() {
        return;
    }
    let el: IRawElementProviderSimple = Text { hwnd, tab }.into();
    for e in events {
        let hr = loaded().map(|api| unsafe {
            match e {
                Event::Focus => (api.raise_event)(el.as_raw(), UIA_AutomationFocusChangedEventId),
                Event::Selection => (api.raise_event)(el.as_raw(), UIA_Text_TextSelectionChangedEventId),
                Event::Text => (api.raise_event)(el.as_raw(), UIA_Text_TextChangedEventId),
                Event::Name(old, new) => {
                    let (old, new) = (VARIANT::from(old.as_str()), VARIANT::from(new.as_str()));
                    // (passed as they are: the caller keeps them, and frees them here)
                    (api.raise_property)(el.as_raw(), UIA_NamePropertyId, std::mem::transmute_copy(&old), std::mem::transmute_copy(&new))
                }
            }
        });
        if super::win::testing() {
            let what = match e {
                Event::Focus => "focus".to_string(),
                Event::Selection => "selection".into(),
                Event::Text => "text".into(),
                Event::Name(_, n) => format!("name {n}"),
            };
            let failed = hr.filter(|h| h.is_err()).map(|h| format!(" (failed: {:#010x})", h.0 as u32)).unwrap_or_default();
            RAISED.with(|r| r.borrow_mut().push(what + &failed));
        }
    }
}

// ---- test mode ----

thread_local! {
    /// The range `uia:` commands work on.
    static PROBE: RefCell<Option<ITextRangeProvider>> = const { RefCell::new(None) };
}

/// The text element as a client finds it: the window's focus, or its first child.
fn probe_text(hwnd: HWND) -> Result<IRawElementProviderFragment> {
    let root: IRawElementProviderFragmentRoot = Root { hwnd }.into();
    unsafe { root.GetFocus() }.or_else(|_| unsafe { root.cast::<IRawElementProviderFragment>()?.Navigate(NavigateDirection_FirstChild) })
}

fn probe() -> Result<ITextRangeProvider> {
    PROBE.with(|p| p.borrow().clone()).ok_or_else(|| Error::new(E_FAIL, "no range yet (uia:caret, say)"))
}

fn unit_named(s: &str) -> Result<TextUnit> {
    Ok(match s {
        "char" => TextUnit_Character,
        "format" => TextUnit_Format,
        "word" => TextUnit_Word,
        "line" => TextUnit_Line,
        "para" => TextUnit_Paragraph,
        "page" => TextUnit_Page,
        "doc" => TextUnit_Document,
        _ => return Err(Error::new(E_INVALIDARG, format!("unit {s}?"))),
    })
}

/// `uia:<op>`: works on a range the way a client does (`print:uia-range` shows it): `caret`, `sel`, `doc`, `visible`,
/// `point=<x>,<y>` (client DIPs) pick one; `expand=<unit>`, `move=<unit>,<n>`, `start=<unit>,<n>` / `end=…`,
/// `find=<text>` (ignoring case; `findback=…`), `select`, `scroll` work on it. Units: char, format, word, line, para,
/// page, doc. Says how far a move went.
pub fn command(cell: &Cell, arg: &str) -> String {
    let hwnd = cell.borrow().hwnd;
    let run = || -> Result<String> {
        let (op, val) = arg.split_once('=').unwrap_or((arg, ""));
        let unit_count = || -> Result<(TextUnit, i32)> {
            let (u, n) = val.split_once(',').unwrap_or((val, "1"));
            Ok((unit_named(u)?, n.trim().parse().map_err(|_| Error::new(E_INVALIDARG, "count?"))?))
        };
        let set = |r: ITextRangeProvider| PROBE.with(|p| *p.borrow_mut() = Some(r));
        let text = || -> Result<ITextProvider2> { probe_text(hwnd)?.cast() };
        unsafe {
            match op {
                "caret" => set(text()?.GetCaretRange(&mut BOOL(0))?),
                "doc" => set(text()?.DocumentRange()?),
                "sel" | "visible" => {
                    let t = text()?;
                    let sa = if op == "sel" { t.GetSelection()? } else { t.GetVisibleRanges()? };
                    let mut first: Option<IUnknown> = None;
                    if !sa.is_null() && (*sa).rgsabound[0].cElements > 0 {
                        first = IUnknown::from_raw_borrowed(&*((*sa).pvData as *const *mut c_void)).cloned();
                    }
                    let _ = windows::Win32::System::Ole::SafeArrayDestroy(sa);
                    set(first.ok_or_else(|| Error::new(E_FAIL, "no range"))?.cast()?);
                }
                "point" => {
                    let (x, y) = val.split_once(',').ok_or_else(|| Error::new(E_INVALIDARG, "x,y?"))?;
                    let (x, y): (f32, f32) = (x.trim().parse().unwrap_or(0.0), y.trim().parse().unwrap_or(0.0));
                    let k = cell.borrow().dpi as f32 / 96.0;
                    let mut p = POINT { x: (x * k).round() as i32, y: (y * k).round() as i32 };
                    let _ = ClientToScreen(hwnd, &mut p);
                    set(text()?.RangeFromPoint(UiaPoint { x: p.x as f64, y: p.y as f64 })?);
                }
                "expand" => probe()?.ExpandToEnclosingUnit(unit_named(val)?)?,
                "move" => {
                    let (u, n) = unit_count()?;
                    return Ok(format!("moved {}", probe()?.Move(u, n)?));
                }
                "start" | "end" => {
                    let (u, n) = unit_count()?;
                    let e = if op == "end" { TextPatternRangeEndpoint_End } else { TextPatternRangeEndpoint_Start };
                    return Ok(format!("moved {}", probe()?.MoveEndpointByUnit(e, u, n)?));
                }
                "find" | "findback" => match probe()?.FindText(&BSTR::from(val), BOOL::from(op == "findback"), BOOL::from(true)) {
                    Ok(r) => set(r),
                    Err(_) => return Ok("not found".into()),
                },
                "select" => probe()?.Select()?,
                "scroll" => probe()?.ScrollIntoView(BOOL::from(true))?,
                _ => return Err(Error::new(E_INVALIDARG, format!("uia:{op}?"))),
            }
        }
        Ok(String::new())
    };
    match run() {
        Ok(s) => s,
        Err(e) => format!("uia failed: {} {}", e.code(), e.message()),
    }
}

/// `print:uia…` values (None for other names): `uia` (the text element: its type, name and whether it has the
/// keyboard), `uia-range` (the probe's text; `uia-len` its length), `uia-line` / `uia-word` / `uia-char` / `uia-para` (the unit at the
/// caret, as screen readers read it), `uia-sel`, `uia-rects` (the probe's rectangles on screen, in client DIPs),
/// `uia-events` (raised since the last time).
pub fn describe(cell: &Cell, what: &str) -> Option<String> {
    if !what.starts_with("uia") {
        return None;
    }
    let hwnd = cell.borrow().hwnd;
    let text_of_range = |r: &ITextRangeProvider| -> Result<String> { Ok(unsafe { r.GetText(-1) }?.to_string()) };
    let run = || -> Result<String> {
        unsafe {
            match what {
                "uia" => {
                    let el: IRawElementProviderSimple = probe_text(hwnd)?.cast()?;
                    let kind = i32::try_from(&el.GetPropertyValue(UIA_ControlTypePropertyId)?).unwrap_or(0);
                    let name = BSTR::try_from(&el.GetPropertyValue(UIA_NamePropertyId)?).unwrap_or_default();
                    let focus = bool::try_from(&el.GetPropertyValue(UIA_HasKeyboardFocusPropertyId)?).unwrap_or(false);
                    let kind = if kind == UIA_DocumentControlTypeId.0 { "document".to_string() } else { kind.to_string() };
                    Ok(format!("{kind} \"{name}\"{}", if focus { ", keyboard" } else { "" }))
                }
                "uia-range" => text_of_range(&probe()?),
                // (how much of a big range a client gets: UTF-16 units)
                "uia-len" => Ok(probe()?.GetText(-1)?.len().to_string()),
                "uia-attrs" => {
                    let r = probe()?;
                    let v = |id| r.GetAttributeValue(id);
                    let font = BSTR::try_from(&v(UIA_FontNameAttributeId)?).map(|b| b.to_string()).unwrap_or_default();
                    let size = f64::try_from(&v(UIA_FontSizeAttributeId)?).unwrap_or(0.0);
                    let read_only = bool::try_from(&v(UIA_IsReadOnlyAttributeId)?).unwrap_or(false);
                    // (not kept: UI Automation's "not supported" object)
                    let weight = if IUnknown::try_from(&v(UIA_FontWeightAttributeId)?).is_ok() { "not supported" } else { "?" };
                    Ok(format!("font {font} size {size} read only {read_only} weight {weight}"))
                }
                "uia-line" | "uia-word" | "uia-char" | "uia-para" => {
                    let t: ITextProvider2 = probe_text(hwnd)?.cast()?;
                    let r = t.GetCaretRange(&mut BOOL(0))?;
                    r.ExpandToEnclosingUnit(unit_named(&what[4..])?)?;
                    text_of_range(&r)
                }
                "uia-sel" => {
                    let t: ITextProvider2 = probe_text(hwnd)?.cast()?;
                    let sa = t.GetSelection()?;
                    let r = IUnknown::from_raw_borrowed(&*((*sa).pvData as *const *mut c_void)).cloned();
                    let _ = windows::Win32::System::Ole::SafeArrayDestroy(sa);
                    text_of_range(&r.ok_or(E_FAIL)?.cast()?)
                }
                "uia-rects" => {
                    let sa = probe()?.GetBoundingRectangles()?;
                    let n = (*sa).rgsabound[0].cElements as usize;
                    let v = std::slice::from_raw_parts((*sa).pvData as *const f64, n).to_vec();
                    let _ = windows::Win32::System::Ole::SafeArrayDestroy(sa);
                    let mut origin = POINT { x: 0, y: 0 };
                    let _ = ClientToScreen(hwnd, &mut origin);
                    let k = cell.borrow().dpi as f64 / 96.0;
                    let parts: Vec<String> = v
                        .chunks(4)
                        .map(|r| {
                            let (x, y) = ((r[0] - origin.x as f64) / k, (r[1] - origin.y as f64) / k);
                            format!("{:.0},{:.0} {:.0}x{:.0}", x, y, r[2] / k, r[3] / k)
                        })
                        .collect();
                    Ok(parts.join(" | "))
                }
                "uia-events" => Ok(RAISED.with(|r| std::mem::take(&mut *r.borrow_mut())).join(", ")),
                _ => Err(Error::new(E_INVALIDARG, format!("print:{what}?"))),
            }
        }
    };
    Some(match run() {
        Ok(s) => s,
        Err(e) => format!("uia failed: {} {}", e.code(), e.message()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::from_text(s.as_bytes())
    }

    /// The units of `s` from the start: each one's text, by moving a range along as a client does.
    fn units(s: &str, u: Unit) -> Vec<String> {
        let d = doc(s);
        let mut t = DocUnits { doc: &d };
        let mut r = enclosing(&mut t, u, 0);
        let mut out = vec![s[r.0 as usize..r.1 as usize].to_string()];
        loop {
            let (next, n) = move_range(&mut t, u, r, 1);
            if n == 0 {
                break;
            }
            assert_eq!(n, 1);
            r = next;
            out.push(s[r.0 as usize..r.1 as usize].to_string());
        }
        out
    }

    #[test]
    fn words_take_the_spaces_after_them() {
        assert_eq!(units("hello world", Unit::Word), ["hello ", "world"]);
        assert_eq!(units("a.b  c", Unit::Word), ["a", ".", "b  ", "c"]);
        // a line starts a word, its indentation too; the line break goes with the word before it
        assert_eq!(units("  foo\r\nbar\n\nbaz", Unit::Word), ["  ", "foo\r\n", "bar\n", "\n", "baz"]);
        // accents stay with their letter
        assert_eq!(units("cafe\u{301} ok", Unit::Word), ["cafe\u{301} ", "ok"]);
    }

    #[test]
    fn the_unit_at_a_place() {
        let d = doc("hello world\nsecond line\n");
        let mut t = DocUnits { doc: &d };
        assert_eq!(enclosing(&mut t, Unit::Word, 0), (0, 6));
        // (at a word's end, the word just passed: the caret after Ctrl+Right)
        assert_eq!(enclosing(&mut t, Unit::Word, 5), (0, 6));
        assert_eq!(enclosing(&mut t, Unit::Word, 6), (6, 12));
        assert_eq!(enclosing(&mut t, Unit::Char, 4), (4, 5));
        assert_eq!(enclosing(&mut t, Unit::Para, 15), (12, 24));
        assert_eq!(enclosing(&mut t, Unit::Doc, 15), (0, 24));
        // at the end: no character or word, the empty last line
        assert_eq!(enclosing(&mut t, Unit::Char, 24), (24, 24));
        assert_eq!(enclosing(&mut t, Unit::Word, 24), (24, 24));
        assert_eq!(enclosing(&mut t, Unit::Para, 24), (24, 24));
        // without a line break at the end, the last line
        let d = doc("one\ntwo");
        let mut t = DocUnits { doc: &d };
        assert_eq!(enclosing(&mut t, Unit::Para, 7), (4, 7));
        assert_eq!(enclosing(&mut t, Unit::Word, 7), (7, 7));
        // an empty document
        let d = doc("");
        let mut t = DocUnits { doc: &d };
        assert_eq!(enclosing(&mut t, Unit::Para, 0), (0, 0));
        assert_eq!(move_range(&mut t, Unit::Para, (0, 0), 1), ((0, 0), 0));
    }

    #[test]
    fn moving_a_range_and_its_ends() {
        let s = "ab\r\nc\u{1F600}d";
        let d = doc(s);
        let mut t = DocUnits { doc: &d };
        // characters: \r\n is one, so is an emoji
        assert_eq!(move_by(&mut t, Unit::Char, 0, 3), (4, 3));
        assert_eq!(move_by(&mut t, Unit::Char, 4, 2), (9, 2));
        assert_eq!(move_by(&mut t, Unit::Char, 0, 100), (10, 6));
        assert_eq!(move_by(&mut t, Unit::Char, 10, -100), (0, -6));
        assert_eq!(move_by(&mut t, Unit::Char, 9, -2), (4, -2));
        // a range moves to the next unit, as a whole; not onto the end
        assert_eq!(move_range(&mut t, Unit::Char, (0, 1), 1), ((1, 2), 1));
        assert_eq!(move_range(&mut t, Unit::Char, (9, 10), 1), ((9, 10), 0));
        assert_eq!(move_range(&mut t, Unit::Para, (0, 4), 1), ((4, 10), 1));
        assert_eq!(move_range(&mut t, Unit::Para, (4, 10), 1), ((4, 10), 0));
        assert_eq!(move_range(&mut t, Unit::Para, (5, 6), -1), ((0, 4), -1));
        // an empty range moves as a caret does, onto the end too
        assert_eq!(move_range(&mut t, Unit::Para, (0, 0), 5), ((10, 10), 2));
        // an end moved past the other takes it along
        assert_eq!(move_endpoint(&mut t, Unit::Char, (4, 5), false, 2), ((9, 9), 2));
        assert_eq!(move_endpoint(&mut t, Unit::Char, (4, 5), true, -2), ((2, 2), -2));
        assert_eq!(move_endpoint(&mut t, Unit::Doc, (4, 5), true, 1), ((4, 10), 1));
    }

    /// The text of a few thousand lines, with `\r\n`, characters of 2, 3 and 4 bytes, and empty lines.
    fn mixed() -> String {
        let parts = ["plain", "caf\u{e9}", "\u{65e5}\u{672c}", "\u{1F600}x", "", "a\tb"];
        let mut s = String::new();
        for i in 0..3000 {
            s.push_str(parts[i % parts.len()]);
            s.push_str(if i % 3 == 0 { "\r\n" } else { "\n" });
        }
        s
    }

    #[test]
    fn many_characters_at_once_as_one_at_a_time() {
        let s = mixed();
        let d = doc(&s);
        let len = d.len();
        // one at a time (by characters: an accent of its own would count too)
        let step = |pos: u64, n: i32| -> (u64, i32) {
            let (mut p, mut k) = (pos, 0);
            while k != n {
                let q = if n > 0 { d.next_char(p) } else { d.prev_char(p) };
                if q == p {
                    break;
                }
                (p, k) = (q, k + n.signum());
            }
            (p, k)
        };
        for &(pos, n) in &[(0, 5000), (0, 30000), (7, 12345), (len, -5000), (len, -40000), (len / 2 + 1, -9999), (len / 3, 8000)] {
            let pos = d.line_start_of(pos);
            assert_eq!(skip_chars(&d, pos, n), step(pos, n), "from {pos} by {n}");
        }
    }

    #[test]
    fn many_paragraphs_at_once_as_one_at_a_time() {
        for s in [mixed(), mixed() + "last", String::new(), "x".to_string(), "\n".to_string()] {
            let d = doc(&s);
            let mut t = DocUnits { doc: &d };
            let len = d.len();
            for pos in [0, 1, len / 2, len.saturating_sub(1), len].map(|p| p.min(len)) {
                for &n in &[1i32, 2, 100, 2000, 5000, -1, -2, -100, -3000, -6000] {
                    let mut stepped = (pos, 0);
                    while stepped.1 != n {
                        let q = if n > 0 { t.next(Unit::Para, stepped.0) } else { t.prev(Unit::Para, stepped.0) };
                        let Some(q) = q else { break };
                        stepped = (q, stepped.1 + n.signum());
                    }
                    assert_eq!(skip_paras(&d, pos, n), Some(stepped), "from {pos} by {n} in {} bytes", len);
                }
            }
        }
    }

    #[test]
    fn text_read_for_a_client() {
        let d = doc("a\u{1F600}b\u{e9}");
        let all: Vec<u16> = "a\u{1F600}b\u{e9}".encode_utf16().collect();
        assert_eq!(text_of(&d, 0, d.len(), -1), all);
        // at most `max` units, never half an emoji
        assert_eq!(text_of(&d, 0, d.len(), 2), &all[..1]);
        assert_eq!(text_of(&d, 0, d.len(), 3), &all[..3]);
        // bytes that aren't UTF-8
        let d = Document::from_text(b"x\xFFy");
        assert_eq!(String::from_utf16_lossy(&text_of(&d, 0, 3, -1)), "x\u{FFFD}y");
        // a long text cut short by `max` doesn't end in a character cut in two (33 bytes of 4-byte emoji)
        let s = "\u{1F600}".repeat(20);
        let d = doc(&s);
        let t = text_of(&d, 0, d.len(), 10);
        assert_eq!(String::from_utf16(&t).unwrap(), "\u{1F600}".repeat(5));
    }
}
