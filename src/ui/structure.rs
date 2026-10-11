//! JSON and XML structure, shown only for those documents: the path bar above the text (where the caret is:
//! `data › [1203] › name`, `catalog › book[3] › title`) and the structure panel (a tree of the document). Both use
//! the lazy child lists of `core::jsonnav` (or `core::xmlnav`, which fills in the same lists for elements), cached
//! per document version. Containers with more than 100 children are grouped into ranges (`[0 … 99]`,
//! `[100 … 199]`, and ranges of ranges), like browser JSON viewers, so even millions of items stay easy to browse.
//! Tree nodes are known by their path (keys and indexes; for XML the elements' places), so what's open stays open
//! across edits, and while a big document is read again after an edit the tree and the path bar keep showing what
//! they had.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::core::document::Document;
use crate::core::job::{Ctx, Job, Notify};
use crate::core::jsonnav::{self, Children, Chunked, Kind, Step};
use crate::core::xmlnav;

use super::gfx::{Align, Gfx, Rect};
use super::highlight::Lang;
use super::theme::Theme;

/// Containers up to this size are scanned right away on the UI thread; bigger ones in the background.
const SYNC_SCAN: u64 = 4 << 20;
/// After an edit to a document bigger than `QUICK`, its structure is read again only once the typing pauses this
/// long (`waiting`): scanning it again after every key took a frame's time for a few MB on the UI thread, and a whole
/// background read of a big file per key. Meanwhile the path and the tree stay as they were (not current).
pub const PAUSE: std::time::Duration = std::time::Duration::from_millis(200);
const QUICK: u64 = 256 << 10;
/// A background scan also keeps the child lists of every container at least this big it passes through.
const KEEP: u64 = 1 << 20;
const BUCKET: u64 = 100;
/// The tree shows this many levels at most (deeper ones would be off to the side anyway); following the caret opens
/// no more than that.
const TREE_DEPTH_MAX: u16 = 100;
pub const ROW_H: f32 = 24.0;
pub const PATH_H: f32 = 28.0;
pub const HEADER_H: f32 = 34.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKey {
    /// A value, by its path (`node_id`).
    Value(u64),
    /// Children `a..b` of the container with that id (`TOP`: the top level, or the root of a normal document).
    Bucket(u64, u64, u64),
}

/// The id of the top level (and of the root value of a normal document, whose children form the tree's first
/// level).
const TOP: u64 = 0;

/// The id of a child of the container with id `parent`: by its key in an object, by its index in an array.
fn node_id(parent: u64, key: Option<&str>, index: u64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut h);
    match key {
        Some(k) => k.hash(&mut h),
        None => index.hash(&mut h),
    }
    h.finish() | 1
}

/// One scan of a JSON or XML container ending at `end` (see `jsonnav::scan`, `xmlnav::scan`).
fn scan_lists(
    xml: bool,
    src: &dyn Chunked,
    open: Option<u64>,
    end: u64,
    keep: u64,
    path_to: Option<u64>,
    ctx: Option<&Ctx>,
) -> Vec<(Option<u64>, Children)> {
    if xml { xmlnav::scan(src, open, Some(end), keep, path_to, ctx) } else { jsonnav::scan(src, open, keep, path_to, ctx) }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: NodeKey,
    pub depth: u16,
    pub label: String,
    pub is_key: bool,
    pub kind: Option<Kind>,
    pub preview: String,
    pub expandable: bool,
    pub expanded: bool,
    pub start: u64,
    pub end: u64,
    pub loading: bool,
}

#[derive(Default)]
pub struct Structure {
    version: u64,
    len: u64,
    cache: HashMap<Option<u64>, Arc<Children>>,
    /// The background scan in progress: which container, and the job.
    pub scanning: Option<(Option<u64>, Job<Vec<(Option<u64>, Children)>>)>,
    /// Path at the caret, and the (caret, version) it is for.
    pub path: Option<Vec<Step>>,
    path_for: Option<(u64, u64)>,
    pub expanded: HashSet<NodeKey>,
    /// What following the caret opened (and the user hasn't touched since): closed again by the next reveal.
    revealed: Vec<NodeKey>,
    pub rows: Vec<Row>,
    rows_dirty: bool,
    /// The document versions `rows` and `path` were worked out for: after a change they show the old text's places
    /// until the new ones are ready, and clicking them mustn't select those places in the new text.
    rows_version: u64,
    path_version: u64,
    pub scroll: f32,
    pub selected: Option<NodeKey>,
    /// Where each path segment was drawn (for clicks).
    pub path_rects: Vec<Rect>,
    /// Reveal the caret's node in the tree after the next path update.
    pub follow: bool,
    pub broken: bool,
    /// The document is XML (else JSON).
    xml: bool,
    /// A document version not read yet (an edit), and when it was first seen (`waiting`).
    edited: Option<(u64, std::time::Instant)>,
    /// An XML document's root element was opened when the tree was first shown.
    root_opened: bool,
}

fn bucket_size(n: u64) -> u64 {
    let mut s = 1;
    while n.div_ceil(s) > BUCKET {
        s *= BUCKET;
    }
    s
}

impl Structure {
    /// Reads the document as `lang` (JSON or XML): everything known about it is forgotten when that changes.
    pub fn set_lang(&mut self, lang: Lang) {
        let xml = lang == Lang::Xml;
        if xml != self.xml {
            *self = Structure { xml, ..Default::default() };
        }
    }

    pub fn is_xml(&self) -> bool {
        self.xml
    }

    /// The path as text to copy: `data[1203].name` for JSON, XPath (`/catalog/book[3]/title`) for XML.
    pub fn path_text(&self, path: &[Step]) -> String {
        if self.xml { xmlnav::xpath(path) } else { jsonnav::path_string(path) }
    }

    /// A child's node id (an XML element by its place, as names repeat).
    fn child_id(&self, parent: u64, key: Option<&str>, index: u64) -> u64 {
        node_id(parent, if self.xml { None } else { key }, index)
    }

    /// Forgets what was read if the document changed. The rows and the path stay until new ones are worked out (for
    /// a big document that takes a background scan), and what's open stays open.
    pub fn sync(&mut self, doc: &Document) {
        if self.version != doc.version || self.len != doc.len() {
            self.version = doc.version;
            self.len = doc.len();
            self.cache.clear();
            self.scanning = None;
            self.path_for = None;
            self.rows_dirty = true;
            self.broken = false;
        }
    }

    pub fn busy(&self) -> bool {
        self.scanning.is_some()
    }

    /// Whether to leave the path and the tree as they are for now: the text changed less than `PAUSE` ago (the
    /// caller looks again then). A document read for the first time, or a small one, doesn't wait.
    pub fn waiting(&mut self, doc: &Document) -> bool {
        if self.version == doc.version || self.version == 0 || doc.len() <= QUICK {
            self.edited = None;
            return false;
        }
        match self.edited {
            Some((v, at)) if v == doc.version => at.elapsed() < PAUSE,
            _ => {
                self.edited = Some((doc.version, std::time::Instant::now()));
                true
            }
        }
    }

    /// The typing pause `waiting` waits for is still going on (test mode waits for it like for a job).
    pub fn pausing(&self) -> bool {
        self.edited.is_some_and(|(_, at)| at.elapsed() < PAUSE)
    }

    /// Whether the tree rows are for the text as it is now (see `rows_version`).
    pub fn rows_current(&self, doc: &Document) -> bool {
        self.rows_version == doc.version
    }

    /// Whether the path bar is for the text as it is now.
    pub fn path_current(&self, doc: &Document) -> bool {
        self.path_version == doc.version
    }

    pub fn progress(&self) -> Option<f32> {
        self.scanning.as_ref().map(|(_, j)| j.fraction())
    }

    fn get(&self, open: Option<u64>) -> Option<Arc<Children>> {
        self.cache.get(&open).cloned()
    }

    /// Makes sure the children of `open`, which ends at `end` (as its parent's list says), are (or will be) known.
    /// Small containers are scanned now. The same scan keeps the lists of the containers on the way to `path_to` (the
    /// caret), so a path many levels deep is worked out from one pass.
    fn ensure(
        &mut self,
        doc: &mut Document,
        open: Option<u64>,
        end: u64,
        path_to: Option<u64>,
        notify: &Notify,
    ) -> Option<Arc<Children>> {
        if let Some(c) = self.get(open) {
            return Some(c);
        }
        if end.saturating_sub(open.unwrap_or(0)) <= SYNC_SCAN && doc.is_ready() {
            let mut own = None;
            for (o, ch) in scan_lists(self.xml, &*doc, open, end, u64::MAX, path_to, None) {
                let ch = Arc::new(ch);
                if o == open {
                    own = Some(ch.clone());
                }
                self.cache.insert(o, ch);
            }
            return own;
        }
        if self.scanning.is_none() {
            let snap = doc.snapshot();
            let xml = self.xml;
            let job = Job::spawn(end.max(1), notify.clone(), move |ctx| scan_lists(xml, &snap, open, end, KEEP, path_to, Some(ctx)));
            self.scanning = Some((open, job));
        }
        None
    }

    /// Picks up a finished background scan; returns true if something new is known.
    pub fn poll(&mut self) -> bool {
        let done = match self.scanning.as_mut() {
            Some((open, job)) => job.take().map(|ch| (*open, ch)),
            None => None,
        };
        match done {
            Some((_, lists)) => {
                self.scanning = None;
                for (open, ch) in lists {
                    if open.is_none() && ch.partial {
                        self.broken = true;
                    }
                    self.cache.insert(open, Arc::new(ch));
                }
                self.path_for = None;
                self.rows_dirty = true;
                true
            }
            None => false,
        }
    }

    /// Recomputes the path at `caret` if needed (scanning what's missing).
    pub fn update_path(&mut self, doc: &mut Document, caret: u64, notify: &Notify) {
        self.sync(doc);
        if self.path_for == Some((caret, doc.version)) {
            return;
        }
        // the last path: for the same text, the walk takes its steps as they are where it goes the same way (what's
        // missing on the way is read in one scan, which keeps the lists of everything up to the caret)
        let old = self.path.take();
        let prev: &[Step] = match &old {
            Some(p) if self.path_version == doc.version => p,
            _ => &[],
        };
        for _ in 0..64 {
            let cache = &self.cache;
            let get = &mut |o: Option<u64>| cache.get(&o).cloned();
            let r = if self.xml {
                xmlnav::path_at(&*doc, caret, prev, get)
            } else {
                jsonnav::path_at(&*doc, caret, prev, get)
            };
            match r {
                Ok(p) => {
                    let changed = old.as_ref() != Some(&p);
                    self.path = Some(p);
                    self.path_for = Some((caret, doc.version));
                    self.path_version = doc.version;
                    if changed && self.follow {
                        self.reveal_path();
                    }
                    return;
                }
                Err((open, end)) => {
                    if self.ensure(doc, open, end, Some(caret), notify).is_none() {
                        // being scanned in the background; keep the old path meanwhile
                        break;
                    }
                }
            }
        }
        self.path = old;
    }

    /// The path at `caret` right now, even when the path bar is hidden. None while a big container on the way is
    /// still being read in the background.
    pub fn path_at_caret(&mut self, doc: &mut Document, caret: u64, notify: &Notify) -> Option<Vec<Step>> {
        self.update_path(doc, caret, notify);
        if self.path_for == Some((caret, doc.version)) { self.path.clone() } else { None }
    }

    /// Expands the tree down to the caret's node and selects it. What the last reveal opened closes again (unless the
    /// user opened or closed it since), so following the caret through thousands of records doesn't leave them all
    /// open (and the tree slow to rebuild).
    fn reveal_path(&mut self) {
        // (no further than the tree shows)
        let Some(path) = self.path.as_ref().map(|p| p.iter().take(TREE_DEPTH_MAX as usize).cloned().collect::<Vec<_>>()) else { return };
        for k in std::mem::take(&mut self.revealed) {
            self.expanded.remove(&k);
        }
        let mut id = TOP;
        // the tree's depth so far (ranges count as levels too)
        let mut depth = 0u16;
        for (n, st) in path.iter().enumerate() {
            if let Some(ch) = self.get(st.parent) {
                // ranges containing this index
                let (mut a, mut b) = (0u64, ch.count);
                while b - a > BUCKET && st.index < b && depth + 1 < TREE_DEPTH_MAX {
                    let size = bucket_size(b - a);
                    let s = a + (st.index - a) / size * size;
                    let e = (s + size).min(b);
                    self.open_by_reveal(NodeKey::Bucket(id, s, e));
                    (a, b, depth) = (s, e, depth + 1);
                }
            }
            id = self.child_id(id, st.key.as_deref(), st.index);
            self.selected = Some(NodeKey::Value(id));
            if n + 1 == path.len() || depth + 1 >= TREE_DEPTH_MAX {
                break;
            }
            self.open_by_reveal(NodeKey::Value(id));
            depth += 1;
        }
        self.rows_dirty = true;
    }

    fn open_by_reveal(&mut self, key: NodeKey) {
        if self.expanded.insert(key) {
            self.revealed.push(key);
        }
    }

    /// The container whose children form the tree's first level (the root object or array of a normal JSON
    /// document, also when the file is cut short; None for JSON Lines, and for XML, whose root element has a name to
    /// show).
    fn root_open(&self, doc: &Document) -> Option<u64> {
        if self.xml {
            return None;
        }
        let top = self.get(None)?;
        let root = top.items.first().filter(|_| top.count == 1)?;
        matches!(doc.byte_at(root.start), Some(b'{' | b'[')).then_some(root.start)
    }

    pub fn toggle(&mut self, key: NodeKey) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.revealed.retain(|k| *k != key);
        self.rows_dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.rows_dirty = true;
    }

    /// Rebuilds the visible rows of the tree if anything changed (the old rows stay until the new ones are ready).
    pub fn rows(&mut self, doc: &mut Document, notify: &Notify) {
        self.sync(doc);
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        let Some(top) = self.ensure(doc, None, doc.len(), None, notify) else {
            self.rows_dirty = true;
            return;
        };
        let (open, list) = match self.root_open(doc) {
            Some(r) => {
                let end = top.items.first().map_or(r, |c| c.end);
                match self.ensure(doc, Some(r), end, None, notify) {
                    Some(l) => (Some(r), l),
                    None => {
                        self.rows_dirty = true;
                        return;
                    }
                }
            }
            None => (None, top),
        };
        if self.xml && !self.root_opened && list.count == 1 {
            // the root element starts open
            self.root_opened = true;
            if list.items.first().is_some_and(xmlnav::has_elements) {
                self.expanded.insert(NodeKey::Value(self.child_id(TOP, None, 0)));
            }
        }
        let n = list.count;
        let mut out = Vec::new();
        self.add(doc, notify, open, TOP, &list, 0, n, 0, &mut out);
        self.rows = out;
        self.rows_version = doc.version;
    }

    /// Adds the rows of children `a..b` of the container at `open` (whose node id is `id`).
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        doc: &mut Document,
        notify: &Notify,
        open: Option<u64>,
        id: u64,
        list: &Arc<Children>,
        a: u64,
        b: u64,
        depth: u16,
        out: &mut Vec<Row>,
    ) {
        if b - a > BUCKET {
            let size = bucket_size(b - a);
            let mut s = a;
            while s < b {
                let e = (s + size).min(b);
                let key = NodeKey::Bucket(id, s, e);
                let expanded = self.expanded.contains(&key);
                let start = list.get(&*doc, s).map_or(0, |c| c.first());
                let end = list.get(&*doc, e - 1).map_or(start, |c| c.end);
                out.push(Row {
                    key,
                    depth,
                    label: format!("[{} … {}]", s, e - 1),
                    is_key: false,
                    kind: None,
                    preview: String::new(),
                    expandable: true,
                    expanded,
                    start,
                    end,
                    loading: false,
                });
                if expanded && depth + 1 < TREE_DEPTH_MAX {
                    self.add(doc, notify, open, id, list, s, e, depth + 1, out);
                } else if expanded {
                    out.push(empty_row(depth + 1, "(deeper levels aren't shown)"));
                }
                s = e;
            }
            return;
        }
        for (k, c) in list.range(&*doc, a, b).into_iter().enumerate() {
            let i = a + k as u64;
            let ((kind, preview), name) = if self.xml {
                (xmlnav::preview(&*doc, &c, 80), Some(xmlnav::name_at(&*doc, c.start)))
            } else {
                (jsonnav::preview(&*doc, &c, 80), c.key_range().map(|(ka, kb)| jsonnav::key_text(&doc.read(ka, kb))))
            };
            let (label, is_key) = match &name {
                Some(k) => (jsonnav::key_label(k), true),
                None => (format!("[{i}]"), false),
            };
            let child = self.child_id(id, name.as_deref(), i);
            let key = NodeKey::Value(child);
            let expandable = kind.is_container();
            let expanded = expandable && self.expanded.contains(&key);
            let size = c.end - c.start;
            let preview = match () {
                _ if !kind.is_container() || size <= 64 * 1024 => preview,
                _ if self.xml && !preview.is_empty() => format!("{preview}  {}", super::app::format_size(size)),
                _ => super::app::format_size(size),
            };
            out.push(Row {
                key,
                depth,
                label,
                is_key,
                kind: Some(kind),
                preview,
                expandable,
                expanded,
                start: c.start,
                end: c.end,
                loading: false,
            });
            if expanded && depth + 1 >= TREE_DEPTH_MAX {
                out.push(empty_row(depth + 1, "(deeper levels aren't shown)"));
            } else if expanded {
                match self.ensure(doc, Some(c.start), c.end, None, notify) {
                    Some(sub) => {
                        let n = sub.count;
                        if n == 0 {
                            out.push(empty_row(depth + 1, if kind == Kind::Object { "(no keys)" } else { "(empty)" }));
                        }
                        self.add(doc, notify, Some(c.start), child, &sub, 0, n, depth + 1, out);
                    }
                    None => {
                        let mut r = empty_row(depth + 1, "Reading…");
                        r.loading = true;
                        out.push(r);
                        self.rows_dirty = true;
                    }
                }
            }
        }
    }

    // ---- painting ----

    #[allow(clippy::too_many_arguments)]
    pub fn paint_path(&mut self, g: &Gfx, t: &Theme, ui: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, r: Rect, hover: Option<usize>, toggle_hover: bool, panel_open: bool, icons: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat) {
        self.path_rects.clear();
        g.fill(r, t.surface);
        g.hline(r.x, r.right(), r.bottom(), true, t.border);
        let toggle = toggle_rect(r);
        if toggle_hover || panel_open {
            g.fill_round(toggle, 4.0, if panel_open { t.pressed } else { t.hover });
        }
        g.text("\u{E8FD}", icons, toggle, t.text_dim, Align::Center);
        let mut x = r.x + 12.0;
        let limit = toggle.x - 8.0;
        let label = |s: &str| -> String { if s.chars().count() > 40 { format!("{}…", s.chars().take(39).collect::<String>()) } else { s.to_string() } };
        let n = self.path.as_ref().map_or(1, |p| p.len().max(1));
        let seg = |i: usize| -> String {
            match &self.path {
                Some(p) if p.is_empty() => "(top)".into(),
                Some(p) => label(&p[i].label()),
                None if self.broken => if self.xml { "Not well-formed XML here" } else { "Not valid JSON here" }.into(),
                None => "…".into(),
            }
        };
        // When it doesn't fit, show the end of the path: only that much is measured (it can be thousands of steps).
        let sep_w = 18.0;
        let mut shown: Vec<(String, f32)> = Vec::new();
        let mut need = 0.0;
        for i in (0..n).rev() {
            let s = seg(i);
            let w = g.measure(&s, ui).0 + 12.0;
            let more = w + if shown.is_empty() { 0.0 } else { sep_w };
            if !shown.is_empty() && need + more > limit - x {
                break;
            }
            need += more;
            shown.push((s, w));
        }
        shown.reverse();
        let skip = n - shown.len();
        if skip > 0 {
            g.text("… ›", ui, Rect::new(x, r.y, 30.0, r.h), t.text_faint, Align::Left);
            x += 30.0;
        }
        self.path_rects.resize(skip, Rect::default());
        for (k, (s, w)) in shown.iter().enumerate() {
            let i = skip + k;
            if k > 0 {
                g.text("›", ui, Rect::new(x, r.y, sep_w, r.h), t.text_faint, Align::Center);
                x += sep_w;
            }
            let br = Rect::new(x, r.y + 3.0, w.min((limit - x).max(0.0)), r.h - 6.0);
            if hover == Some(i) {
                g.fill_round(br, 4.0, t.hover);
            }
            let last = i + 1 == n;
            g.text(s, ui, br, if last { t.text } else { t.text_dim }, Align::Center);
            self.path_rects.push(br);
            x += w;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint_panel(
        &mut self,
        g: &Gfx,
        t: &Theme,
        ui: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        bold: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        icons: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        r: Rect,
        hover_row: Option<usize>,
        close_hover: bool,
        bar_hot: bool,
    ) {
        g.fill(r, t.surface);
        g.vline(r.x, r.y, r.bottom(), false, t.border);
        let head = Rect::new(r.x, r.y, r.w, HEADER_H);
        g.text("Structure", bold, Rect::new(r.x + 14.0, r.y, r.w - 60.0, HEADER_H), t.text, Align::Left);
        if let Some(p) = self.progress().filter(|_| !self.rows.is_empty()) {
            // reading the document again after an edit; the rows shown are from before it
            g.text(&format!("Updating… {:.0}%", p * 100.0), ui, Rect::new(r.x + 14.0, r.y, r.w - 60.0, HEADER_H), t.text_faint, Align::Right);
        }
        let close = close_rect(r);
        if close_hover {
            g.fill_round(close, 4.0, t.hover);
        }
        g.text("\u{E711}", icons, close, t.text_dim, Align::Center);
        g.hline(r.x, r.right(), head.bottom(), true, t.border);
        let body = body_rect(r);
        g.push_clip(body);
        if let Some(p) = self.progress() {
            if self.rows.is_empty() {
                g.text(&format!("Reading the structure… {:.0}%", p * 100.0), ui, Rect::new(body.x + 14.0, body.y + 6.0, body.w - 20.0, ROW_H), t.text_dim, Align::Left);
            }
        } else if self.rows.is_empty() {
            let msg = match (self.broken, self.xml) {
                (true, false) => "This doesn't look like valid JSON.",
                (true, true) => "This doesn't look like well-formed XML.",
                _ => "Nothing to show.",
            };
            g.text(msg, ui, Rect::new(body.x + 14.0, body.y + 6.0, body.w - 20.0, ROW_H), t.text_dim, Align::Left);
        }
        let max_scroll = (self.rows.len() as f32 * ROW_H - body.h + ROW_H).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let first = (self.scroll / ROW_H).floor() as usize;
        let count = (body.h / ROW_H).ceil() as usize + 1;
        for (k, row) in self.rows.iter().enumerate().skip(first).take(count) {
            let y = body.y + k as f32 * ROW_H - self.scroll;
            let rr = Rect::new(body.x, y, body.w, ROW_H);
            if self.selected == Some(row.key) {
                g.fill(g.snap_rect(rr), t.selection_inactive);
            } else if hover_row == Some(k) {
                g.fill(g.snap_rect(rr), t.hover);
            }
            let x0 = body.x + 8.0 + row.depth as f32 * 14.0;
            if row.expandable {
                let glyph = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
                g.text(glyph, icons, Rect::new(x0, y, 16.0, ROW_H), t.text_dim, Align::Center);
            }
            let tx = x0 + 18.0;
            let label_color = if row.loading || row.kind.is_none() && !row.expandable {
                t.text_dim
            } else if row.is_key && matches!(row.kind, Some(Kind::Element | Kind::Leaf)) {
                t.syn_tag
            } else if row.is_key {
                t.syn_key
            } else {
                t.text_dim
            };
            let (lw, _) = g.measure(&row.label, ui);
            let lw = lw.min((rr.right() - tx - 8.0).max(0.0));
            g.text(&row.label, ui, Rect::new(tx, y, lw + 2.0, ROW_H), label_color, Align::Left);
            let px = tx + lw + 8.0;
            let (ptext, pcolor) = match row.kind {
                Some(Kind::Object) => (if row.preview.is_empty() { "{…}".to_string() } else { format!("{{…}} {}", row.preview) }, t.text_faint),
                Some(Kind::Array) => (if row.preview.is_empty() { "[…]".to_string() } else { format!("[…] {}", row.preview) }, t.text_faint),
                Some(Kind::String) => (row.preview.clone(), t.syn_string),
                Some(Kind::Number) => (row.preview.clone(), t.syn_number),
                Some(Kind::Bool) | Some(Kind::Null) => (row.preview.clone(), t.syn_literal),
                Some(Kind::Other) => (row.preview.clone(), t.text_dim),
                // an element's attributes; one without elements inside shows its text too
                Some(Kind::Element) => (row.preview.clone(), t.syn_attr),
                Some(Kind::Leaf) => (row.preview.clone(), t.syn_string),
                None => (String::new(), t.text_dim),
            };
            if !ptext.is_empty() && px < rr.right() - 12.0 {
                g.text(&ptext, ui, Rect::new(px, y, rr.right() - px - 8.0, ROW_H), pcolor, Align::Left);
            }
        }
        g.pop_clip();
        // the scrollbar's thumb, as the text's (wider under the mouse)
        if let Some((_, thumb, _)) = self.scrollbar(r) {
            let w = if bar_hot { 8.0 } else { 5.0 };
            let color = if bar_hot { t.scroll_thumb_hover } else { t.scroll_thumb };
            g.fill_round(Rect::new(thumb.right() - w - 3.0, thumb.y + 2.0, w, thumb.h - 4.0), w / 2.0, color);
        }
    }

    /// The panel's scrollbar while its rows don't all fit: the track (down the right edge of the rows), the thumb in
    /// it, and how far the rows scroll.
    pub fn scrollbar(&self, panel: Rect) -> Option<(Rect, Rect, f32)> {
        let body = body_rect(panel);
        // (as in `paint_panel`: a row's room left after the last one)
        let max = self.rows.len() as f32 * ROW_H - body.h + ROW_H;
        if max <= 0.0 || body.h <= 0.0 {
            return None;
        }
        let bar_w = super::theme::metrics::SCROLLBAR_W;
        let track = Rect::new(body.right() - bar_w, body.y, bar_w, body.h);
        let h = (body.h * body.h / (body.h + max)).max(28.0).min(body.h);
        let y = body.y + (body.h - h) * (self.scroll / max).clamp(0.0, 1.0);
        Some((track, Rect::new(track.x, y, track.w, h), max))
    }

    /// Row index at a point in the panel body.
    pub fn row_at(&self, panel: Rect, x: f32, y: f32) -> Option<(usize, bool)> {
        let body = body_rect(panel);
        if !body.contains(x, y) {
            return None;
        }
        let k = ((y - body.y + self.scroll) / ROW_H).floor() as usize;
        let row = self.rows.get(k)?;
        let x0 = body.x + 8.0 + row.depth as f32 * 14.0;
        let on_chevron = row.expandable && x >= x0 - 4.0 && x < x0 + 18.0;
        Some((k, on_chevron))
    }

    /// Scrolls the panel so the selected row is visible.
    pub fn scroll_to_selected(&mut self, panel: Rect) {
        let Some(sel) = self.selected else { return };
        let Some(k) = self.rows.iter().position(|r| r.key == sel) else { return };
        let body = body_rect(panel);
        let y = k as f32 * ROW_H;
        if y < self.scroll {
            self.scroll = (y - ROW_H * 2.0).max(0.0);
        } else if y + ROW_H > self.scroll + body.h {
            self.scroll = y + ROW_H * 3.0 - body.h;
        }
    }
}

fn empty_row(depth: u16, text: &str) -> Row {
    Row {
        key: NodeKey::Bucket(u64::MAX, u64::MAX, u64::MAX),
        depth,
        label: text.into(),
        is_key: false,
        kind: None,
        preview: String::new(),
        expandable: false,
        expanded: false,
        start: 0,
        end: 0,
        loading: false,
    }
}

pub fn toggle_rect(path_bar: Rect) -> Rect {
    Rect::new(path_bar.right() - 40.0, path_bar.y + 2.0, 30.0, path_bar.h - 4.0)
}

pub fn close_rect(panel: Rect) -> Rect {
    Rect::new(panel.right() - 38.0, panel.y + 5.0, 28.0, HEADER_H - 10.0)
}

pub fn body_rect(panel: Rect) -> Rect {
    Rect::new(panel.x + 1.0, panel.y + HEADER_H, panel.w - 1.0, panel.h - HEADER_H)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{EditKind, Sel};

    fn notify() -> Notify {
        Arc::new(|| {})
    }

    fn row(s: &Structure, label: &str) -> Option<Row> {
        s.rows.iter().find(|r| r.label == label).cloned()
    }

    fn edit(d: &mut Document, at: u64, text: &[u8]) {
        d.begin(EditKind::Other, Sel::at(at));
        d.insert(at, text);
        d.end(Sel::at(at));
    }

    fn wait(s: &mut Structure) {
        while s.busy() {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn open_nodes_stay_open_across_edits() {
        let n = notify();
        let mut d = Document::from_text(br#"{"meta": {"n": 3}, "data": [{"id": 1}, {"id": 2}]}"#);
        let mut s = Structure::default();
        s.rows(&mut d, &n);
        s.toggle(row(&s, "data").unwrap().key);
        s.rows(&mut d, &n);
        assert!(row(&s, "[1]").is_some());
        // everything after the edit moves, but "data" is still the same node
        edit(&mut d, 15, b"12");
        s.rows(&mut d, &n);
        assert!(row(&s, "data").unwrap().expanded);
        assert_eq!(row(&s, "[1]").unwrap().start, d.len() - 11);
    }

    #[test]
    fn following_the_caret_doesnt_leave_everything_open() {
        let n = notify();
        let text: String = (0..500).map(|i| format!("{{\"i\":{i},\"v\":[1,2]}}\n")).collect();
        let mut d = Document::from_text(text.as_bytes());
        let mut s = Structure { follow: true, ..Default::default() };
        let user = NodeKey::Value(node_id(TOP, None, 7));
        s.toggle(user);
        for i in [3, 250, 499] {
            let caret = text.find(&format!("\"i\":{i},")).unwrap() + 5;
            s.update_path(&mut d, caret as u64, &n);
            assert_eq!(jsonnav::path_string(s.path.as_ref().unwrap()), format!("[{i}].i"));
        }
        // the last record and the range holding it, and what the user opened
        let want: HashSet<NodeKey> = [user, NodeKey::Value(node_id(TOP, None, 499)), NodeKey::Bucket(TOP, 400, 500)].into();
        assert_eq!(s.expanded, want);
        assert_eq!(s.selected, Some(NodeKey::Value(node_id(node_id(TOP, None, 499), Some("i"), 0))));
    }

    #[test]
    fn the_panels_scrollbar_shows_where_the_rows_are() {
        let n = notify();
        let text = format!("[{}]", (0..60).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        let mut d = Document::from_text(text.as_bytes());
        let mut s = Structure::default();
        s.rows(&mut d, &n);
        assert_eq!(s.rows.len(), 60);
        // room for 10 rows: the thumb at the top, then at the bottom
        let panel = Rect::new(0.0, 0.0, 300.0, HEADER_H + 10.0 * ROW_H);
        let (track, thumb, max) = s.scrollbar(panel).unwrap();
        assert_eq!((thumb.y, max), (track.y, 51.0 * ROW_H));
        s.scroll = max;
        let (track, thumb, _) = s.scrollbar(panel).unwrap();
        assert!((thumb.bottom() - track.bottom()).abs() < 0.01);
        // when they all fit, none
        assert!(s.scrollbar(Rect::new(0.0, 0.0, 300.0, HEADER_H + 80.0 * ROW_H)).is_none());
    }

    #[test]
    fn a_file_cut_short_still_shows_its_tree() {
        let n = notify();
        let mut d = Document::from_text(br#"{"a": 1, "b": [1, 2"#);
        let mut s = Structure::default();
        s.rows(&mut d, &n);
        assert_eq!(s.rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        s.update_path(&mut d, 18, &n);
        assert_eq!(jsonnav::path_string(s.path.as_ref().unwrap()), "b[1]");
    }

    fn labels(s: &Structure) -> Vec<&str> {
        s.rows.iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    fn xml_documents_show_their_elements() {
        let n = notify();
        let text = "<?xml version=\"1.0\"?>\n<catalog>\n  <book id=\"1\"><title>A</title></book>\n  <book id=\"2\"><title>B</title></book>\n</catalog>\n";
        let mut d = Document::from_text(text.as_bytes());
        let mut s = Structure::default();
        s.set_lang(Lang::Xml);
        s.rows(&mut d, &n);
        // the root element starts open
        assert_eq!(labels(&s), ["catalog", "book", "book"]);
        assert_eq!((s.rows[1].kind, s.rows[1].preview.as_str()), (Some(Kind::Element), "id=\"1\""));
        s.follow = true;
        s.update_path(&mut d, text.find("B<").unwrap() as u64, &n);
        assert_eq!(s.path_text(s.path.as_ref().unwrap()), "/catalog/book[2]/title");
        s.rows(&mut d, &n);
        assert_eq!(labels(&s), ["catalog", "book", "book", "title"]);
        assert_eq!((s.rows[3].kind, s.rows[3].preview.as_str()), (Some(Kind::Leaf), "B"));
        assert_eq!(s.selected, Some(s.rows[3].key));
        // the root closed by the user stays closed
        s.toggle(s.rows[0].key);
        s.rows(&mut d, &n);
        assert_eq!(labels(&s), ["catalog"]);
        // read as JSON, it's another document
        s.set_lang(Lang::Json);
        assert!(s.rows.is_empty() && s.path.is_none());
    }

    #[test]
    fn deep_paths_are_worked_out_in_one_go() {
        let n = notify();
        let deep = 20_000;
        let docs = [
            (Lang::Xml, format!("{}x{}", "<a>".repeat(deep), "</a>".repeat(deep))),
            (Lang::Json, format!("{}1{}", "[".repeat(deep), "]".repeat(deep))),
        ];
        for (lang, text) in docs {
            let mut d = Document::from_text(text.as_bytes());
            let mut s = Structure::default();
            s.set_lang(lang);
            // (in the innermost one)
            let caret = text.find(['x', '1']).unwrap() as u64 + 1;
            let start = std::time::Instant::now();
            s.update_path(&mut d, caret, &n);
            assert_eq!(s.path.as_ref().map(|p| p.len()), Some(deep), "{lang:?}");
            // the caret moving inside it: the path's steps are taken as they were
            s.update_path(&mut d, caret - 1, &n);
            assert!(s.path_current(&d) && s.path.as_ref().unwrap().len() == deep);
            // the panel, following the caret, opens only as many levels as the tree shows
            let mut s = Structure::default();
            s.set_lang(lang);
            s.follow = true;
            s.update_path(&mut d, caret, &n);
            s.rows(&mut d, &n);
            assert!(s.rows.len() <= TREE_DEPTH_MAX as usize + 2, "{lang:?}: {} rows", s.rows.len());
            assert!(s.rows.iter().any(|r| Some(r.key) == s.selected));
            assert!(start.elapsed().as_secs_f64() < 2.0, "{lang:?}: {:?}", start.elapsed());
        }
    }

    #[test]
    fn a_big_xml_document_is_read_in_the_background() {
        let n = notify();
        let items: String = (0..150_000).map(|i| format!("<item n=\"{i}\">text {i}</item>\n")).collect();
        let text = format!("<items>\n{items}</items>\n");
        assert!(text.len() as u64 > SYNC_SCAN);
        let mut d = Document::from_text(text.as_bytes());
        let mut s = Structure::default();
        s.set_lang(Lang::Xml);
        let caret = text.find("text 123456<").unwrap() as u64;
        while s.path.is_none() || s.rows.is_empty() {
            s.update_path(&mut d, caret, &n);
            s.rows(&mut d, &n);
            wait(&mut s);
        }
        assert_eq!(s.path_text(s.path.as_ref().unwrap()), "/items/item[123457]");
        assert_eq!(labels(&s)[..3], ["items", "[0 … 9999]", "[10000 … 19999]"]);
    }

    #[test]
    fn rows_and_path_stay_while_a_big_document_is_read_again() {
        let n = notify();
        let items: Vec<String> = (0..200_000).map(|i| format!("{{\"id\": {i}, \"name\": \"n{i}\"}}")).collect();
        let text = format!("{{\"meta\": 1, \"data\": [{}]}}", items.join(", "));
        assert!(text.len() as u64 > SYNC_SCAN);
        let mut d = Document::from_text(text.as_bytes());
        let mut s = Structure::default();
        let caret = text.find("\"n7\"").unwrap() as u64 + 1;
        while s.path.is_none() || s.rows.is_empty() {
            s.update_path(&mut d, caret, &n);
            s.rows(&mut d, &n);
            wait(&mut s);
        }
        let before = (s.rows.clone().len(), s.path.clone());
        edit(&mut d, 10, b"0");
        s.update_path(&mut d, caret + 1, &n);
        s.rows(&mut d, &n);
        assert!(s.busy(), "a big document is read again in the background");
        assert_eq!((s.rows.len(), s.path.clone()), before);
        wait(&mut s);
        s.update_path(&mut d, caret + 1, &n);
        s.rows(&mut d, &n);
        assert_eq!(jsonnav::path_string(s.path.as_ref().unwrap()), "data[7].name");
    }
}
