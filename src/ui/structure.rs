//! The JSON and XML path bar and structure panel drawn on Windows, and where their parts are (the model, what they
//! show, is `crate::structure`, shared with Linux).

pub use crate::structure::*;

use crate::core::jsonnav::Kind;

use super::gfx::{Align, Gfx, Rect};
use super::theme::Theme;

pub const ROW_H: f32 = 24.0;
pub const PATH_H: f32 = 28.0;
pub const HEADER_H: f32 = 34.0;

impl Structure {
    // ---- painting ----

    #[allow(clippy::too_many_arguments)]
    pub fn paint_path(&mut self, g: &Gfx, t: &Theme, ui: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, r: Rect, hover: Option<usize>, toggle_hover: bool, panel_open: bool, icons: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, rects: &mut Vec<Rect>) {
        rects.clear();
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
                None if self.broken => if self.is_xml() { "Not well-formed XML here" } else { "Not valid JSON here" }.into(),
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
        rects.resize(skip, Rect::default());
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
            rects.push(br);
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
            let msg = match (self.broken, self.is_xml()) {
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
    use crate::core::document::Document;
    use crate::core::job::Notify;
    use std::sync::Arc;

    #[test]
    fn the_panels_scrollbar_shows_where_the_rows_are() {
        let n: Notify = Arc::new(|| {});
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

}
