//! JSON structure, shown only for JSON documents: the path bar above the text (where the caret is:
//! `data › [1203] › name`) and the structure panel (a tree of the document). Both use `core::jsonnav`'s lazy child
//! lists, cached per document version. Containers with more than 100 children are grouped into ranges
//! (`[0 … 99]`, `[100 … 199]`, and ranges of ranges), like browser JSON viewers, so even millions of items stay
//! easy to browse.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::document::Document;
use crate::core::job::{Job, Notify};
use crate::core::jsonnav::{self, Children, Kind, Step};

use super::gfx::{Align, Gfx, Rect};
use super::theme::Theme;

/// Containers up to this size are scanned right away on the UI thread; bigger ones in the background.
const SYNC_SCAN: u64 = 4 << 20;
/// A background scan also keeps the child lists of every container at least this big it passes through.
const KEEP: u64 = 1 << 20;
const BUCKET: u64 = 100;
pub const ROW_H: f32 = 24.0;
pub const PATH_H: f32 = 28.0;
pub const HEADER_H: f32 = 34.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKey {
    /// A value, by where it starts.
    Value(u64),
    /// Children `a..b` of the container at `parent` (None = top level).
    Bucket(Option<u64>, u64, u64),
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
    pub rows: Vec<Row>,
    rows_dirty: bool,
    pub scroll: f32,
    pub selected: Option<NodeKey>,
    /// Where each path segment was drawn (for clicks).
    pub path_rects: Vec<Rect>,
    /// Reveal the caret's node in the tree after the next path update.
    pub follow: bool,
    pub broken: bool,
}

fn bucket_size(n: u64) -> u64 {
    let mut s = 1;
    while n.div_ceil(s) > BUCKET {
        s *= BUCKET;
    }
    s
}

impl Structure {
    /// Forgets everything if the document changed.
    pub fn sync(&mut self, doc: &Document) {
        if self.version != doc.version || self.len != doc.len() {
            self.version = doc.version;
            self.len = doc.len();
            self.cache.clear();
            self.scanning = None;
            self.path = None;
            self.path_for = None;
            self.rows_dirty = true;
            self.broken = false;
        }
    }

    pub fn busy(&self) -> bool {
        self.scanning.is_some()
    }

    pub fn progress(&self) -> Option<f32> {
        self.scanning.as_ref().map(|(_, j)| j.fraction())
    }

    fn get(&self, open: Option<u64>) -> Option<Arc<Children>> {
        self.cache.get(&open).cloned()
    }

    /// Makes sure the children of `open` are (or will be) known. Small containers are scanned now.
    fn ensure(&mut self, doc: &mut Document, open: Option<u64>, size: u64, notify: &Notify) -> Option<Arc<Children>> {
        if let Some(c) = self.get(open) {
            return Some(c);
        }
        if size <= SYNC_SCAN && doc.is_ready() {
            let ch = Arc::new(jsonnav::children(&*doc, open, None));
            self.cache.insert(open, ch.clone());
            return Some(ch);
        }
        if self.scanning.is_none() {
            let snap = doc.snapshot();
            let total = match open {
                None => snap.len(),
                Some(o) => size.max(1) + o,
            };
            let job = Job::spawn(total, notify.clone(), move |ctx| jsonnav::scan(&snap, open, KEEP, Some(ctx)));
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
        for _ in 0..64 {
            let cache = &self.cache;
            let r = jsonnav::path_at(&*doc, caret, &mut |o| cache.get(&o).cloned());
            match r {
                Ok(p) => {
                    let changed = self.path.as_ref() != Some(&p);
                    self.path = Some(p);
                    self.path_for = Some((caret, doc.version));
                    if changed && self.follow {
                        self.reveal_path();
                    }
                    return;
                }
                Err(open) => {
                    let size = self.size_of(doc, open);
                    if self.ensure(doc, open, size, notify).is_none() {
                        // being scanned in the background; keep the old path meanwhile
                        return;
                    }
                }
            }
        }
    }

    /// The path at `caret` right now, even when the path bar is hidden. None while a big container on the way is
    /// still being read in the background.
    pub fn path_at_caret(&mut self, doc: &mut Document, caret: u64, notify: &Notify) -> Option<Vec<Step>> {
        self.update_path(doc, caret, notify);
        if self.path_for == Some((caret, doc.version)) { self.path.clone() } else { None }
    }

    /// Byte size of a container (from its parent's list), or the whole document for the top level.
    fn size_of(&self, doc: &Document, open: Option<u64>) -> u64 {
        match open {
            None => doc.len(),
            Some(o) => self
                .cache
                .values()
                .find_map(|ch| ch.find_start(doc, o).map(|c| c.end - o))
                .unwrap_or(doc.len() - o),
        }
    }

    /// Expands the tree down to the caret's node and selects it.
    fn reveal_path(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let mut parent: Option<u64> = self.root_open();
        for st in &path {
            if let Some(ch) = self.get(st.parent) {
                // ranges containing this index
                let n = ch.count;
                let (mut a, mut b) = (0u64, n);
                while b - a > BUCKET {
                    let size = bucket_size(b - a);
                    let s = a + (st.index - a) / size * size;
                    let e = (s + size).min(b);
                    self.expanded.insert(NodeKey::Bucket(st.parent, s, e));
                    a = s;
                    b = e;
                }
            }
            self.selected = Some(NodeKey::Value(st.start));
            if Some(st.start) != parent {
                parent = Some(st.start);
            }
        }
        for st in path.iter().take(path.len().saturating_sub(1)) {
            self.expanded.insert(NodeKey::Value(st.start));
        }
        self.rows_dirty = true;
    }

    /// The container whose children form the tree's first level (the root object or array of a normal JSON
    /// document; None for JSON Lines).
    fn root_open(&self) -> Option<u64> {
        let top = self.get(None)?;
        if top.count == 1 && !top.partial { top.items.first().map(|c| c.start) } else { None }
    }

    pub fn toggle(&mut self, key: NodeKey) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.rows_dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.rows_dirty = true;
    }

    /// Rebuilds the visible rows of the tree if anything changed.
    pub fn rows(&mut self, doc: &mut Document, notify: &Notify) {
        self.sync(doc);
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        self.rows.clear();
        let Some(top) = self.ensure(doc, None, doc.len(), notify) else {
            self.rows_dirty = true;
            return;
        };
        let (open, list) = match self.root_open() {
            Some(r) => {
                let size = top.items.first().map_or(0, |c| c.end) - r;
                match self.ensure(doc, Some(r), size, notify) {
                    Some(l) => (Some(r), l),
                    None => {
                        self.rows_dirty = true;
                        return;
                    }
                }
            }
            None => (None, top),
        };
        let n = list.count;
        let mut out = Vec::new();
        self.add(doc, notify, open, &list, 0, n, 0, &mut out);
        self.rows = out;
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        doc: &mut Document,
        notify: &Notify,
        open: Option<u64>,
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
                let key = NodeKey::Bucket(open, s, e);
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
                if expanded {
                    self.add(doc, notify, open, list, s, e, depth + 1, out);
                }
                s = e;
            }
            return;
        }
        for (k, c) in list.range(&*doc, a, b).into_iter().enumerate() {
            let i = a + k as u64;
            let (kind, preview) = jsonnav::preview(&*doc, &c, 80);
            let (label, is_key) = match c.key_range() {
                Some((ka, kb)) => (jsonnav::key_text(&doc.read(ka, kb)), true),
                None => (format!("[{i}]"), false),
            };
            let key = NodeKey::Value(c.start);
            let expandable = kind.is_container();
            let expanded = expandable && self.expanded.contains(&key);
            let size = c.end - c.start;
            let preview = if kind.is_container() && size > 64 * 1024 { super::app::format_size(size) } else { preview };
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
            if expanded {
                match self.ensure(doc, Some(c.start), size, notify) {
                    Some(sub) => {
                        let n = sub.count;
                        if n == 0 {
                            out.push(empty_row(depth + 1, if kind == Kind::Object { "(no keys)" } else { "(empty)" }));
                        }
                        self.add(doc, notify, Some(c.start), &sub, 0, n, depth + 1, out);
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
        g.line(r.x, r.bottom() - 0.5, r.right(), r.bottom() - 0.5, t.border, 1.0);
        let toggle = toggle_rect(r);
        if toggle_hover || panel_open {
            g.fill_round(toggle, 4.0, if panel_open { t.pressed } else { t.hover });
        }
        g.text("\u{E8FD}", icons, toggle, t.text_dim, Align::Center);
        let mut x = r.x + 12.0;
        let limit = toggle.x - 8.0;
        let label = |s: &str| -> String { if s.chars().count() > 40 { format!("{}…", s.chars().take(39).collect::<String>()) } else { s.to_string() } };
        let segs: Vec<String> = match &self.path {
            Some(p) if p.is_empty() => vec!["(top)".into()],
            Some(p) => p.iter().map(|s| label(&s.label())).collect(),
            None if self.broken => vec!["Not valid JSON here".into()],
            None => vec!["…".into()],
        };
        // When it doesn't fit, show the end of the path.
        let widths: Vec<f32> = segs.iter().map(|s| g.measure(s, ui).0 + 12.0).collect();
        let sep_w = 18.0;
        let total: f32 = widths.iter().sum::<f32>() + sep_w * segs.len().saturating_sub(1) as f32;
        let mut skip = 0;
        let mut need = total;
        while need > limit - x && skip + 1 < segs.len() {
            need -= widths[skip] + sep_w;
            skip += 1;
        }
        if skip > 0 {
            g.text("… ›", ui, Rect::new(x, r.y, 30.0, r.h), t.text_faint, Align::Left);
            x += 30.0;
        }
        for (i, s) in segs.iter().enumerate() {
            if i < skip {
                self.path_rects.push(Rect::default());
                continue;
            }
            if i > skip {
                g.text("›", ui, Rect::new(x, r.y, sep_w, r.h), t.text_faint, Align::Center);
                x += sep_w;
            }
            let w = widths[i];
            let br = Rect::new(x, r.y + 3.0, w.min((limit - x).max(0.0)), r.h - 6.0);
            if hover == Some(i) {
                g.fill_round(br, 4.0, t.hover);
            }
            let last = i + 1 == segs.len();
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
    ) {
        g.fill(r, t.surface);
        g.line(r.x + 0.5, r.y, r.x + 0.5, r.bottom(), t.border, 1.0);
        let head = Rect::new(r.x, r.y, r.w, HEADER_H);
        g.text("Structure", bold, Rect::new(r.x + 14.0, r.y, r.w - 60.0, HEADER_H), t.text, Align::Left);
        let close = close_rect(r);
        if close_hover {
            g.fill_round(close, 4.0, t.hover);
        }
        g.text("\u{E711}", icons, close, t.text_dim, Align::Center);
        g.line(r.x, head.bottom() - 0.5, r.right(), head.bottom() - 0.5, t.border, 1.0);
        let body = body_rect(r);
        g.push_clip(body);
        if let Some(p) = self.progress() {
            if self.rows.is_empty() {
                g.text(&format!("Reading the structure… {:.0}%", p * 100.0), ui, Rect::new(body.x + 14.0, body.y + 6.0, body.w - 20.0, ROW_H), t.text_dim, Align::Left);
            }
        } else if self.rows.is_empty() {
            let msg = if self.broken { "This doesn't look like valid JSON." } else { "Nothing to show." };
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
                g.fill(rr, t.selection_inactive);
            } else if hover_row == Some(k) {
                g.fill(rr, t.hover);
            }
            let x0 = body.x + 8.0 + row.depth as f32 * 14.0;
            if row.expandable {
                let glyph = if row.expanded { "\u{E70D}" } else { "\u{E76C}" };
                g.text(glyph, icons, Rect::new(x0, y, 16.0, ROW_H), t.text_dim, Align::Center);
            }
            let tx = x0 + 18.0;
            let label_color = if row.loading || row.kind.is_none() && !row.expandable {
                t.text_dim
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
                None => (String::new(), t.text_dim),
            };
            if !ptext.is_empty() && px < rr.right() - 12.0 {
                g.text(&ptext, ui, Rect::new(px, y, rr.right() - px - 8.0, ROW_H), pcolor, Align::Left);
            }
        }
        g.pop_clip();
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
        key: NodeKey::Bucket(None, u64::MAX, u64::MAX),
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
