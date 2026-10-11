//! File → Print on Linux: GTK's print dialog and print operation (printers, page ranges, copies, Print to File). The
//! pages are laid out by `crate::print` when GTK says printing begins (the paper is known then) and drawn with Pango
//! and cairo, in points.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{cairo, pango};

use super::view::set_color;
use super::{Ui, get_ui};
use crate::core::buffer::Snapshot;
use crate::edit::{group, plural};
use crate::highlight::Lang;
use crate::print::{self, Row, Shape, Text, TooMany};
use crate::theme::{Theme, rgb, tok_color};

/// The text and how it looks, taken from the tab when Print is chosen; the pages once GTK says printing begins.
struct Job {
    snap: Snapshot,
    lang: Lang,
    title: String,
    /// The whole text: its length and last line's number; the selection: its range, its first and last line's
    /// numbers and where its first line starts.
    len: u64,
    last_line: u64,
    sel: Option<(u64, u64, u64, u64, u64)>,
    font: String,
    size: f64,
    numbers: bool,
    tab: usize,
    theme: Theme,
    pages: Option<Pages>,
    too_many: bool,
    printed: u64,
}

/// The rows and where they go on each page.
struct Pages {
    rows: Vec<Row>,
    per_page: usize,
    count: usize,
    font: pango::FontDescription,
    small: pango::FontDescription,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    body_top: f64,
    line_h: f64,
    small_h: f64,
    char_w: f64,
    digits: usize,
    gutter: usize,
}

fn font(family: &str, points: f64) -> pango::FontDescription {
    let mut f = pango::FontDescription::from_string(family);
    f.set_size((points * pango::SCALE as f64) as i32);
    f
}

/// Prints the active tab: GTK's print dialog, or (`export`: a PDF file, and whether to print the selection) straight
/// into a PDF file without one (tests).
pub fn run(ui: &Ui, export: Option<(PathBuf, bool)>) {
    let job = {
        let mut a = ui.app.borrow_mut();
        let s = &a.settings;
        let (font, size, numbers, tab) = (s.font.clone(), s.font_size as f64, s.line_numbers, s.tab_size as usize);
        let t = a.tab_mut();
        let snap = t.doc.snapshot();
        let sel = t.view.sel;
        let line_of = |pos: u64| t.doc.line_of(pos).unwrap_or(0) + 1;
        let sel = (!sel.is_empty()).then(|| (sel.start(), sel.end(), line_of(sel.start()), line_of(sel.end()), t.doc.line_start_of(sel.start())));
        Job {
            len: snap.len(),
            last_line: line_of(snap.len()),
            snap,
            lang: t.lang,
            title: t.title(),
            sel,
            font,
            size: size.clamp(4.0, 72.0),
            numbers,
            tab,
            theme: Theme::light(rgb(0x0067C0)),
            pages: None,
            too_many: false,
            printed: 0,
        }
    };
    let has_sel = job.sel.is_some();
    let title = job.title.clone();
    let job = Rc::new(RefCell::new(job));
    let op = gtk4::PrintOperation::new();
    op.set_job_name(&title);
    op.set_unit(gtk4::Unit::Points);
    op.set_embed_page_setup(true);
    op.set_has_selection(has_sel);
    op.set_support_selection(true);
    if let Some(s) = ui.print_settings.borrow().as_ref() {
        op.set_print_settings(Some(s));
    }
    let j = job.clone();
    op.connect_begin_print(move |op, ctx| {
        let mut j = j.borrow_mut();
        let selection = op.print_settings().is_some_and(|s| s.print_pages() == gtk4::PrintPages::Selection);
        let (from, to, line, last, line_start) = match j.sel {
            Some((a, b, l, last, ls)) if selection => (a, b, l, last, ls),
            _ => (0, j.len, 1, j.last_line, 0),
        };
        // three quarters of an inch from the paper's edges, or the printer's own margin if it's wider
        let setup = ctx.page_setup();
        let m = |margin: f64| (54.0 - margin).max(0.0);
        let (w, h) = (ctx.width(), ctx.height());
        let (left, right) = (m(setup.left_margin(gtk4::Unit::Points)), w - m(setup.right_margin(gtk4::Unit::Points)));
        let (top, bottom) = (m(setup.top_margin(gtk4::Unit::Points)), h - m(setup.bottom_margin(gtk4::Unit::Points)));
        let layout = ctx.create_pango_layout();
        let measure = |f: &pango::FontDescription| {
            layout.set_font_description(Some(f));
            layout.set_text("MMMMMMMMMM");
            let (w, h) = layout.size();
            (w as f64 / pango::SCALE as f64 / 10.0, h as f64 / pango::SCALE as f64)
        };
        let (normal, small) = (font(&j.font, j.size), font(&j.font, j.size * 0.85));
        let (char_w, line_h) = measure(&normal);
        let (_, small_h) = measure(&small);
        let (char_w, line_h) = (char_w.max(0.5), line_h.max(1.0));
        // the file's name, a blank line, the text, a blank line, the page's number
        let body_top = top + 2.0 * small_h;
        let per_page = (((bottom - 2.0 * small_h - body_top) / line_h).floor() as usize).max(1);
        let digits = if j.numbers { last.max(1).to_string().len() } else { 0 };
        let gutter = if j.numbers { digits + 2 } else { 0 };
        let cols = (((right - left) / char_w).floor() as usize).saturating_sub(gutter).max(1);
        let state = print::state_at(&j.snap, j.lang, line_start, from);
        let text = Text { snap: &j.snap, from, to, line, lang: j.lang, state };
        match print::layout(&text, Shape { cols, rows: per_page, tab: j.tab }, &mut |_| true) {
            Ok(Some(rows)) => {
                let count = rows.len().div_ceil(per_page);
                op.set_n_pages(count as i32);
                j.pages = Some(Pages { rows, per_page, count, font: normal, small, left, right, top, bottom, body_top, line_h, small_h, char_w, digits, gutter });
            }
            Ok(None) => op.cancel(),
            Err(TooMany) => {
                j.too_many = true;
                op.cancel();
            }
        }
    });
    let j = job.clone();
    op.connect_draw_page(move |_, ctx, n| {
        let mut j = j.borrow_mut();
        j.printed += 1;
        let j = &*j;
        let Some(p) = &j.pages else { return };
        let cr = ctx.cairo_context();
        let layout = ctx.create_pango_layout();
        let show = |cr: &cairo::Context, x: f64, y: f64, layout: &pango::Layout| {
            cr.move_to(x, y);
            pangocairo::functions::show_layout(cr, layout);
        };
        // the file's name at the top, the page's number at the bottom
        layout.set_font_description(Some(&p.small));
        set_color(&cr, j.theme.text_dim);
        layout.set_text(&j.title);
        show(&cr, p.left, p.top, &layout);
        layout.set_text(&format!("Page {} of {}", group(n as u64 + 1), group(p.count as u64)));
        let w = layout.size().0 as f64 / pango::SCALE as f64;
        show(&cr, p.left + (p.right - p.left - w) / 2.0, p.bottom - p.small_h, &layout);
        layout.set_font_description(Some(&p.font));
        let first = n as usize * p.per_page;
        for (r, row) in p.rows.iter().skip(first).take(p.per_page).enumerate() {
            let y = p.body_top + r as f64 * p.line_h;
            if let (Some(l), true) = (row.line, p.digits > 0) {
                let s = l.to_string();
                set_color(&cr, j.theme.text_dim);
                layout.set_attributes(None);
                layout.set_text(&s);
                show(&cr, p.left + p.digits.saturating_sub(s.len()) as f64 * p.char_w, y, &layout);
            }
            // colors, bold and italic as attributes of the row's text, which goes in one piece
            let attrs = pango::AttrList::new();
            for run in &row.runs {
                let Some(tok) = run.tok else { continue };
                let c = tok_color(&j.theme, tok);
                let ch = |s: u32| (((c >> s) & 0xFF) * 257) as u16;
                let mut color = pango::AttrColor::new_foreground(ch(16), ch(8), ch(0));
                color.set_start_index(run.start as u32);
                color.set_end_index(run.end as u32);
                attrs.insert(color);
                if print::bold(tok) || print::italic(tok) {
                    let mut a = if print::bold(tok) { pango::AttrInt::new_weight(pango::Weight::Bold) } else { pango::AttrInt::new_style(pango::Style::Italic) };
                    a.set_start_index(run.start as u32);
                    a.set_end_index(run.end as u32);
                    attrs.insert(a);
                }
            }
            set_color(&cr, j.theme.text);
            layout.set_text(&row.text);
            layout.set_attributes(Some(&attrs));
            show(&cr, p.left + p.gutter as f64 * p.char_w, y, &layout);
            layout.set_attributes(None);
        }
    });
    let j = job.clone();
    let exporting = export.is_some();
    op.connect_done(move |op, result| {
        let Some(ui) = get_ui() else { return };
        let j = j.borrow();
        if j.too_many {
            let msg = format!("That's more than {} pages: select the part to print, then print the selection.", group(print::PAGES_MAX));
            ui.with(|a| a.flash(msg, true));
        } else if result == gtk4::PrintOperationResult::Apply {
            if !exporting {
                ui.print_settings.replace(op.print_settings());
            }
            let msg = format!("Sent {} to the printer", plural(j.printed, "page", "pages"));
            ui.with(|a| a.flash(msg, false));
        } else if result == gtk4::PrintOperationResult::Error {
            ui.with(|a| a.flash("Couldn't print.", true));
        }
    });
    let action = match &export {
        Some((path, selection)) => {
            op.set_export_filename(path);
            if *selection {
                let s = gtk4::PrintSettings::new();
                s.set_print_pages(gtk4::PrintPages::Selection);
                op.set_print_settings(Some(&s));
            }
            gtk4::PrintOperationAction::Export
        }
        None => gtk4::PrintOperationAction::PrintDialog,
    };
    if let Err(e) = op.run(action, Some(&ui.window)) {
        ui.with(|a| a.flash(format!("Couldn't print: {e}"), true));
    }
}
