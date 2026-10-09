//! The tab strip and the status bar, drawn like Slate's on Windows (cairo and Pango), with what's where for
//! clicks and hovering.

use gtk4::cairo;
use gtk4::pango;

use super::app::App;
use super::view::set_color;

pub const TAB_H: f64 = 36.0;
pub const STATUS_H: f64 = 26.0;
const TAB_MIN_W: f64 = 96.0;
const TAB_MAX_W: f64 = 240.0;
const PLUS_W: f64 = 36.0;
const CLOSE_W: f64 = 22.0;

/// What's under the pointer in the tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPart {
    Tab(usize),
    Close(usize),
    Plus,
    /// The arrows to scroll tabs that don't fit.
    Left,
    Right,
}

/// Where the tabs are drawn: (tab, x, width), the "+" button's x, and whether there are more than fit.
#[derive(Default, Clone)]
pub struct TabStrip {
    pub tabs: Vec<(usize, f64, f64)>,
    pub plus: f64,
    pub arrows: bool,
    pub width: f64,
}

/// How the tabs are laid out in `width`, starting at tab `first` (when they don't all fit).
pub fn tab_strip(app: &App, width: f64, first: &mut usize) -> TabStrip {
    let n = app.tabs.len().max(1);
    let arrows_w = 2.0 * 28.0;
    let avail = (width - PLUS_W - 8.0).max(TAB_MIN_W);
    let w = (avail / n as f64).clamp(TAB_MIN_W, TAB_MAX_W);
    let fit = ((avail / w).floor() as usize).max(1);
    let mut strip = TabStrip { width, ..Default::default() };
    if fit >= n {
        *first = 0;
        for i in 0..n {
            strip.tabs.push((i, i as f64 * w, w));
        }
        strip.plus = n as f64 * w;
        return strip;
    }
    // more than fit: arrows at the right, and the active tab always among those shown
    let fit = (((avail - arrows_w) / TAB_MIN_W).floor() as usize).max(1);
    let w = ((avail - arrows_w) / fit as f64).min(TAB_MAX_W);
    if app.active < *first {
        *first = app.active;
    } else if app.active >= *first + fit {
        *first = app.active + 1 - fit;
    }
    *first = (*first).min(n - fit);
    for (k, i) in (*first..*first + fit).enumerate() {
        strip.tabs.push((i, k as f64 * w, w));
    }
    strip.plus = fit as f64 * w;
    strip.arrows = true;
    strip
}

pub fn tab_part_at(strip: &TabStrip, x: f64) -> Option<TabPart> {
    for &(i, tx, w) in &strip.tabs {
        if x >= tx && x < tx + w {
            let close = tx + w - CLOSE_W - 6.0;
            return Some(if x >= close && x < close + CLOSE_W { TabPart::Close(i) } else { TabPart::Tab(i) });
        }
    }
    if x >= strip.plus && x < strip.plus + PLUS_W {
        return Some(TabPart::Plus);
    }
    if strip.arrows {
        let right = strip.width - 28.0;
        if x >= right - 28.0 && x < right {
            return Some(TabPart::Left);
        }
        if x >= right {
            return Some(TabPart::Right);
        }
    }
    None
}

fn text_layout(ctx: &pango::Context, font: &pango::FontDescription, text: &str, max_w: f64) -> pango::Layout {
    let l = pango::Layout::new(ctx);
    l.set_font_description(Some(font));
    l.set_text(text);
    l.set_width((max_w.max(1.0) * pango::SCALE as f64) as i32);
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_single_paragraph_mode(true);
    l
}

fn draw_text(cr: &cairo::Context, l: &pango::Layout, x: f64, y_center: f64, color: u32) {
    let (_, h) = l.pixel_size();
    set_color(cr, color);
    cr.move_to(x, (y_center - h as f64 / 2.0).round());
    pangocairo::functions::show_layout(cr, l);
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
    cr.close_path();
}

/// Draws the tab strip.
pub fn paint_tabs(cr: &cairo::Context, ctx: &pango::Context, app: &App, strip: &TabStrip, height: f64, hover: Option<TabPart>) {
    let t = &app.theme;
    set_color(cr, t.frame);
    cr.paint().ok();
    let mut font = pango::FontDescription::from_string("Sans 10");
    if let Some(f) = ctx.font_description() {
        font.set_family(&f.family().unwrap_or_else(|| "Sans".into()));
    }
    for &(i, x, w) in &strip.tabs {
        let tab = &app.tabs[i];
        let active = i == app.active;
        let top = 6.0;
        if active {
            set_color(cr, t.surface);
            rounded(cr, x + 2.0, top, w - 4.0, height - top + 8.0, 8.0);
            let _ = cr.fill();
        } else if matches!(hover, Some(TabPart::Tab(j)) | Some(TabPart::Close(j)) if j == i) {
            set_color(cr, t.hover);
            rounded(cr, x + 2.0, top + 2.0, w - 4.0, height - top - 6.0, 6.0);
            let _ = cr.fill();
        } else if i + 1 != app.active && i + 1 < app.tabs.len() {
            // a thin line between two tabs that aren't the active one
            set_color(cr, t.border);
            cr.rectangle((x + w - 1.0).round(), top + 8.0, 1.0, height - top - 16.0);
            let _ = cr.fill();
        }
        let mut title = tab.title();
        if tab.doc.is_dirty() {
            title = format!("• {title}");
        }
        let text_w = w - 14.0 - CLOSE_W - 10.0;
        let l = text_layout(ctx, &font, &title, text_w);
        let color = if active { t.text } else { t.text_dim };
        draw_text(cr, &l, x + 14.0, top + (height - top) / 2.0, color);
        // close button: on the active tab and the one under the pointer
        let cx = x + w - CLOSE_W - 6.0;
        let cy = top + (height - top - CLOSE_W) / 2.0;
        if hover == Some(TabPart::Close(i)) {
            set_color(cr, t.hover);
            rounded(cr, cx, cy, CLOSE_W, CLOSE_W, 4.0);
            let _ = cr.fill();
        }
        if active || matches!(hover, Some(TabPart::Tab(j)) | Some(TabPart::Close(j)) if j == i) {
            set_color(cr, t.text_dim);
            cr.set_line_width(1.2);
            let (mx, my, s) = (cx + CLOSE_W / 2.0, cy + CLOSE_W / 2.0, 4.0);
            cr.move_to(mx - s, my - s);
            cr.line_to(mx + s, my + s);
            cr.move_to(mx + s, my - s);
            cr.line_to(mx - s, my + s);
            let _ = cr.stroke();
        }
    }
    // the "+" button
    let (px, py) = (strip.plus + 4.0, 6.0 + (height - 6.0 - 28.0) / 2.0);
    if hover == Some(TabPart::Plus) {
        set_color(cr, t.hover);
        rounded(cr, px, py, 28.0, 28.0, 6.0);
        let _ = cr.fill();
    }
    set_color(cr, t.text_dim);
    cr.set_line_width(1.2);
    let (mx, my) = (px + 14.0, py + 14.0);
    cr.move_to(mx - 6.0, my);
    cr.line_to(mx + 6.0, my);
    cr.move_to(mx, my - 6.0);
    cr.line_to(mx, my + 6.0);
    let _ = cr.stroke();
    if strip.arrows {
        let right = strip.width - 28.0;
        for (k, x) in [right - 28.0, right].into_iter().enumerate() {
            let my = 6.0 + (height - 6.0) / 2.0;
            set_color(cr, t.text_dim);
            let mx = x + 14.0;
            let d = if k == 0 { -1.0 } else { 1.0 };
            cr.move_to(mx - 2.0 * d, my - 5.0);
            cr.line_to(mx + 3.0 * d, my);
            cr.line_to(mx - 2.0 * d, my + 5.0);
            let _ = cr.stroke();
        }
    }
}

/// The status bar's texts: what's at the left (where the caret is, or a message), and the items at the right.
pub fn status_texts(app: &App) -> (String, Vec<String>, bool) {
    let tab = app.tab();
    let doc = &tab.doc;
    let mut bad = false;
    let left = if let Some((msg, b)) = &app.flash {
        bad = *b;
        msg.clone()
    } else if tab.loading() {
        "Opening…".into()
    } else if let Some((done, total)) = doc.index_progress() {
        format!("Reading lines… {}%", if total > 0 { done * 100 / total } else { 0 })
    } else if tab.saving() {
        "Saving…".into()
    } else {
        let sel = tab.view.sel;
        let pos = sel.caret;
        let line = doc.line_of(pos).map(|l| l + 1);
        let ls = doc.line_start_of(pos);
        let col = if pos - ls <= 4 << 20 {
            let bytes = doc.read(ls, pos);
            String::from_utf8_lossy(&bytes).chars().count() as u64 + 1
        } else {
            pos - ls + 1
        };
        let mut s = match line {
            Some(l) => format!("Ln {l}, Col {col}"),
            None => format!("Col {col}"),
        };
        if !sel.is_empty() {
            let n = sel.end() - sel.start();
            if n <= 4 << 20 {
                let chars = String::from_utf8_lossy(&doc.read(sel.start(), sel.end())).chars().count();
                s.push_str(&format!("  ({chars} selected)"));
            } else {
                s.push_str(&format!("  ({} selected)", size_text(n)));
            }
        }
        s
    };
    let indent = match tab.indent {
        crate::edit::Indent::Tabs => "Tabs".to_string(),
        crate::edit::Indent::Spaces(n) => format!("Spaces: {n}"),
    };
    let right = vec![indent, tab.lang.label().to_string(), doc.eol.short().to_string(), doc.encoding.label(), size_text(doc.len())];
    (left, right, bad)
}

pub fn size_text(n: u64) -> String {
    match n {
        0..1024 => format!("{n} bytes"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        _ => format!("{:.2} GB", n as f64 / 1_073_741_824.0),
    }
}

/// What's in the status bar where: (item index, x, width) for the items at the right.
pub fn paint_status(cr: &cairo::Context, ctx: &pango::Context, app: &App, width: f64, height: f64) -> Vec<(usize, f64, f64)> {
    let t = &app.theme;
    set_color(cr, t.frame);
    cr.paint().ok();
    set_color(cr, t.border);
    cr.rectangle(0.0, 0.0, width, 1.0);
    let _ = cr.fill();
    let font = pango::FontDescription::from_string("Sans 9");
    let (left, right, bad) = status_texts(app);
    let mut items = Vec::new();
    let mut x = width - 12.0;
    for (k, s) in right.iter().enumerate().rev() {
        let l = text_layout(ctx, &font, s, 400.0);
        let w = l.pixel_size().0 as f64;
        x -= w;
        draw_text(cr, &l, x, height / 2.0, t.text_dim);
        items.push((k, x - 8.0, w + 16.0));
        x -= 24.0;
    }
    let l = text_layout(ctx, &font, &left, (x - 16.0).max(40.0));
    draw_text(cr, &l, 12.0, height / 2.0, if bad { t.error } else { t.text_dim });
    items
}
