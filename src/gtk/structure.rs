//! The JSON and XML path bar and structure panel drawn on Linux (cairo and Pango, as the tab strip is), and where
//! their parts are for clicks. What they show is `crate::structure`, shared with Windows.

use gtk4::{cairo, pango};

use crate::core::jsonnav::Kind;
use crate::structure::Structure;
use crate::theme::Theme;

use super::chrome::{draw_text, rounded, text_layout};
use super::view::set_color;

pub const ROW_H: f64 = 24.0;
pub const PATH_H: f64 = 28.0;
pub const HEADER_H: f64 = 34.0;
/// The panel's width at least, and the part of its left edge that resizes it.
pub const PANEL_MIN: f64 = 180.0;
pub const SPLITTER_W: f64 = 5.0;

/// A part of the path bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathPart {
    Seg(usize),
    /// The button at its end that shows or hides the panel.
    Toggle,
}

fn ui_font(ctx: &pango::Context) -> pango::FontDescription {
    let mut f = pango::FontDescription::from_string("Sans 9");
    if let Some(d) = ctx.font_description() {
        f.set_family(&d.family().unwrap_or_else(|| "Sans".into()));
    }
    f
}

/// The path bar's show/hide panel button.
fn toggle_rect(w: f64, h: f64) -> (f64, f64, f64, f64) {
    (w - 40.0, 2.0, 30.0, h - 4.0)
}

/// The panel's close button and the rows' area (from the top).
fn close_rect(w: f64) -> (f64, f64, f64, f64) {
    (w - 38.0, 5.0, 28.0, HEADER_H - 10.0)
}

/// What's at `x` in the path bar, by where its parts were drawn last (`segs`).
pub fn path_part_at(segs: &[(f64, f64)], w: f64, h: f64, x: f64) -> Option<PathPart> {
    let (tx, _, tw, _) = toggle_rect(w, h);
    if x >= tx && x < tx + tw {
        return Some(PathPart::Toggle);
    }
    segs.iter().position(|&(a, sw)| x >= a && x < a + sw).map(PathPart::Seg)
}

/// Draws the path bar (`data › [1203] › name`) in `w` x `h`; returns each segment's (x, width), for clicks (those
/// left out at the start when it doesn't fit are (0, 0)).
#[allow(clippy::too_many_arguments)]
pub fn paint_path(cr: &cairo::Context, ctx: &pango::Context, s: &Structure, t: &Theme, w: f64, h: f64, hover: Option<PathPart>, panel_open: bool) -> Vec<(f64, f64)> {
    set_color(cr, t.surface);
    cr.paint().ok();
    set_color(cr, t.border);
    cr.rectangle(0.0, h - 1.0, w, 1.0);
    let _ = cr.fill();
    let font = ui_font(ctx);
    // the panel's button: a window with a side panel
    let (tx, ty, tw, th) = toggle_rect(w, h);
    if hover == Some(PathPart::Toggle) || panel_open {
        set_color(cr, if panel_open { t.pressed } else { t.hover });
        rounded(cr, tx, ty, tw, th, 4.0);
        let _ = cr.fill();
    }
    set_color(cr, t.text_dim);
    cr.set_line_width(1.0);
    let (ix, iy) = ((tx + tw / 2.0 - 7.0).round() + 0.5, (ty + th / 2.0 - 6.0).round() + 0.5);
    cr.rectangle(ix, iy, 14.0, 12.0);
    cr.move_to(ix + 9.0, iy);
    cr.line_to(ix + 9.0, iy + 12.0);
    let _ = cr.stroke();
    let label = |st: String| -> String { if st.chars().count() > 40 { format!("{}…", st.chars().take(39).collect::<String>()) } else { st } };
    let n = s.path.as_ref().map_or(1, |p| p.len().max(1));
    let seg = |i: usize| -> String {
        match &s.path {
            Some(p) if p.is_empty() => "(top)".into(),
            Some(p) => label(p[i].label()),
            None if s.broken => if s.is_xml() { "Not well-formed XML here" } else { "Not valid JSON here" }.into(),
            None => "…".into(),
        }
    };
    // When it doesn't fit, the end of the path shows: only that much is measured (it can be thousands of steps).
    let (mut x, limit, sep_w) = (12.0, tx - 8.0, 18.0);
    let mut shown: Vec<(pango::Layout, f64)> = Vec::new();
    let mut need = 0.0;
    for i in (0..n).rev() {
        let l = text_layout(ctx, &font, &seg(i), 4000.0);
        let sw = l.pixel_size().0 as f64 + 12.0;
        let more = sw + if shown.is_empty() { 0.0 } else { sep_w };
        if !shown.is_empty() && need + more > limit - x {
            break;
        }
        need += more;
        shown.push((l, sw));
    }
    shown.reverse();
    let skip = n - shown.len();
    let mid = h / 2.0;
    if skip > 0 {
        draw_text(cr, &text_layout(ctx, &font, "… ›", 40.0), x, mid, t.text_faint);
        x += 30.0;
    }
    let mut segs = vec![(0.0, 0.0); skip];
    for (k, (l, sw)) in shown.iter().enumerate() {
        let i = skip + k;
        if k > 0 {
            let sep = text_layout(ctx, &font, "›", 40.0);
            draw_text(cr, &sep, x + (sep_w - sep.pixel_size().0 as f64) / 2.0, mid, t.text_faint);
            x += sep_w;
        }
        let bw = sw.min((limit - x).max(0.0));
        if hover == Some(PathPart::Seg(i)) {
            set_color(cr, t.hover);
            rounded(cr, x, 3.0, bw, h - 6.0, 4.0);
            let _ = cr.fill();
        }
        l.set_width(((bw - 12.0).max(1.0) * pango::SCALE as f64) as i32);
        draw_text(cr, l, x + 6.0, mid, if i + 1 == n { t.text } else { t.text_dim });
        segs.push((x, bw));
        x += sw;
    }
    segs
}

/// Draws the structure panel in `w` x `h` (its rows scrolled by `s.scroll`, kept in range).
pub fn paint_panel(cr: &cairo::Context, ctx: &pango::Context, s: &mut Structure, t: &Theme, w: f64, h: f64, hover_row: Option<usize>, close_hover: bool) {
    set_color(cr, t.surface);
    cr.paint().ok();
    set_color(cr, t.border);
    cr.rectangle(0.0, 0.0, 1.0, h);
    let _ = cr.fill();
    let font = ui_font(ctx);
    let mut bold = font.clone();
    bold.set_weight(pango::Weight::Semibold);
    draw_text(cr, &text_layout(ctx, &bold, "Structure", (w - 60.0).max(1.0)), 14.0, HEADER_H / 2.0, t.text);
    if let Some(p) = s.progress().filter(|_| !s.rows.is_empty()) {
        // read again after an edit; the rows shown are from before it
        let l = text_layout(ctx, &font, &format!("Updating… {:.0}%", p * 100.0), 200.0);
        draw_text(cr, &l, w - 50.0 - l.pixel_size().0 as f64, HEADER_H / 2.0, t.text_faint);
    }
    let (cx, cy, cw, ch) = close_rect(w);
    if close_hover {
        set_color(cr, t.hover);
        rounded(cr, cx, cy, cw, ch, 4.0);
        let _ = cr.fill();
    }
    set_color(cr, t.text_dim);
    cr.set_line_width(1.2);
    let (mx, my) = (cx + cw / 2.0, cy + ch / 2.0);
    cr.move_to(mx - 4.5, my - 4.5);
    cr.line_to(mx + 4.5, my + 4.5);
    cr.move_to(mx + 4.5, my - 4.5);
    cr.line_to(mx - 4.5, my + 4.5);
    let _ = cr.stroke();
    set_color(cr, t.border);
    cr.rectangle(0.0, HEADER_H, w, 1.0);
    let _ = cr.fill();
    let body_h = (h - HEADER_H).max(0.0);
    cr.save().ok();
    cr.rectangle(1.0, HEADER_H + 1.0, w - 1.0, body_h);
    cr.clip();
    let message = if let Some(p) = s.progress() {
        s.rows.is_empty().then(|| format!("Reading the structure… {:.0}%", p * 100.0))
    } else if s.rows.is_empty() {
        Some(match (s.broken, s.is_xml()) {
            (true, false) => "This doesn't look like valid JSON.".into(),
            (true, true) => "This doesn't look like well-formed XML.".into(),
            _ => "Nothing to show.".into(),
        })
    } else {
        None
    };
    if let Some(m) = message {
        draw_text(cr, &text_layout(ctx, &font, &m, (w - 24.0).max(1.0)), 14.0, HEADER_H + 6.0 + ROW_H / 2.0, t.text_dim);
    }
    clamp_scroll(s, body_h);
    let first = (s.scroll as f64 / ROW_H).floor() as usize;
    let count = (body_h / ROW_H).ceil() as usize + 1;
    for (k, row) in s.rows.iter().enumerate().skip(first).take(count) {
        let y = HEADER_H + k as f64 * ROW_H - s.scroll as f64;
        if s.selected == Some(row.key) || hover_row == Some(k) {
            set_color(cr, if s.selected == Some(row.key) { t.selection_inactive } else { t.hover });
            cr.rectangle(1.0, y, w - 1.0, ROW_H);
            let _ = cr.fill();
        }
        let x0 = 8.0 + row.depth as f64 * 14.0;
        let mid = y + ROW_H / 2.0;
        if row.expandable {
            // ▸ closed, ▾ open
            set_color(cr, t.text_dim);
            let cx = x0 + 8.0;
            if row.expanded {
                cr.move_to(cx - 4.0, mid - 2.0);
                cr.line_to(cx + 4.0, mid - 2.0);
                cr.line_to(cx, mid + 3.0);
            } else {
                cr.move_to(cx - 2.0, mid - 4.0);
                cr.line_to(cx + 3.0, mid);
                cr.line_to(cx - 2.0, mid + 4.0);
            }
            cr.close_path();
            let _ = cr.fill();
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
        let l = text_layout(ctx, &font, &row.label, (w - tx - 8.0).max(1.0));
        let lw = l.pixel_size().0 as f64;
        draw_text(cr, &l, tx, mid, label_color);
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
        if !ptext.is_empty() && px < w - 12.0 {
            draw_text(cr, &text_layout(ctx, &font, &ptext, w - px - 8.0), px, mid, pcolor);
        }
    }
    cr.restore().ok();
    // the scrollbar's thumb, as the text's
    if let Some((ty, th)) = thumb(s, body_h) {
        set_color(cr, t.scroll_thumb);
        rounded(cr, w - 8.0, HEADER_H + ty + 2.0, 5.0, th - 4.0, 2.5);
        let _ = cr.fill();
    }
}

/// How far the rows scroll at most in a body `body_h` high (a row's room left after the last one).
fn max_scroll(s: &Structure, body_h: f64) -> f64 {
    (s.rows.len() as f64 * ROW_H - body_h + ROW_H).max(0.0)
}

fn clamp_scroll(s: &mut Structure, body_h: f64) {
    s.scroll = (s.scroll as f64).clamp(0.0, max_scroll(s, body_h)) as f32;
}

/// The thumb's top and height in the body, while the rows don't all fit.
fn thumb(s: &Structure, body_h: f64) -> Option<(f64, f64)> {
    let max = max_scroll(s, body_h);
    if max <= 0.0 || body_h <= 0.0 {
        return None;
    }
    let th = (body_h * body_h / (body_h + max)).max(28.0).min(body_h);
    Some(((body_h - th) * (s.scroll as f64 / max).clamp(0.0, 1.0), th))
}

/// What's at (`x`, `y`) in the panel: its close button, the scrollbar (the part of the rows a click there scrolls
/// to, 0 to 1), a row (and whether on its chevron).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PanelPart {
    Splitter,
    Close,
    Bar(f64),
    Row(usize, bool),
}

pub fn panel_part_at(s: &Structure, w: f64, h: f64, x: f64, y: f64) -> Option<PanelPart> {
    if x < SPLITTER_W {
        return Some(PanelPart::Splitter);
    }
    let (cx, cy, cw, ch) = close_rect(w);
    if x >= cx && x < cx + cw && y >= cy && y < cy + ch {
        return Some(PanelPart::Close);
    }
    if y < HEADER_H {
        return None;
    }
    let body_h = (h - HEADER_H).max(0.0);
    if x >= w - 12.0 && thumb(s, body_h).is_some() {
        return Some(PanelPart::Bar(((y - HEADER_H) / body_h).clamp(0.0, 1.0)));
    }
    let k = ((y - HEADER_H + s.scroll as f64) / ROW_H).floor() as usize;
    let row = s.rows.get(k)?;
    let x0 = 8.0 + row.depth as f64 * 14.0;
    Some(PanelPart::Row(k, row.expandable && x >= x0 - 4.0 && x < x0 + 18.0))
}

/// Scrolls the panel (`body_h` of rows shown) to `f` (0 the top, 1 the end).
pub fn scroll_to(s: &mut Structure, body_h: f64, f: f64) {
    s.scroll = (f.clamp(0.0, 1.0) * max_scroll(s, body_h)) as f32;
}

/// Scrolls by `rows` (a wheel's turn).
pub fn scroll_by(s: &mut Structure, body_h: f64, rows: f64) {
    s.scroll = (s.scroll as f64 + rows * ROW_H).clamp(0.0, max_scroll(s, body_h)) as f32;
}

/// Scrolls the panel so the selected row shows.
pub fn scroll_to_selected(s: &mut Structure, body_h: f64) {
    let Some(sel) = s.selected else { return };
    let Some(k) = s.rows.iter().position(|r| r.key == sel) else { return };
    let y = k as f64 * ROW_H;
    let top = s.scroll as f64;
    if y < top {
        s.scroll = (y - ROW_H * 2.0).max(0.0) as f32;
    } else if y + ROW_H > top + body_h {
        s.scroll = (y + ROW_H * 3.0 - body_h) as f32;
    }
}
