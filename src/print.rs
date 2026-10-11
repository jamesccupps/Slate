//! Printing, shared by both windows: the text in rows of a fixed number of columns (a line too long for one goes on
//! in the rows after it), each row with its colors, as many rows to a page as fit. The windows measure their font,
//! show the system's print dialog and draw the pages (`ui::print` with GDI, `gtk::print` with cairo and Pango).

use crate::core::buffer::Snapshot;
use crate::core::text::{decode_char, display_char, is_continuation};
use crate::highlight::{self, Lang, State, Tok};

/// Documents up to this size are colored as in the editor, each line in the state the line before it ended in; in
/// bigger ones each line is colored on its own (as the editor does there too).
pub const HL_MAX: u64 = 32 << 20;
/// Print asks before printing more pages than this...
pub const PAGES_ASK: u64 = 100;
/// ...and prints no more than this many (the selection can be printed instead).
pub const PAGES_MAX: u64 = 10_000;

/// The text is read this much at a time; a longer line is colored and laid out in pieces of `PIECE` bytes.
const CHUNK: usize = 1 << 20;
const PIECE: usize = 64 << 10;

/// The paper, in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Columns in a row (a wide character takes two).
    pub cols: usize,
    /// Rows on a page.
    pub rows: usize,
    /// A tab goes on to the next multiple of this many columns from the start of its line.
    pub tab: usize,
}

/// One printed row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The number of the line it starts (1 = the first); None for the rest of a line too long for one row.
    pub line: Option<u64>,
    /// Tabs as spaces, control characters as visible symbols, bytes that aren't UTF-8 as U+FFFD.
    pub text: String,
    /// The parts of `text` in one color, in order.
    pub runs: Vec<Run>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// Byte range of the row's text.
    pub start: usize,
    pub end: usize,
    /// The column it starts at.
    pub col: usize,
    /// What it is (None: plain text).
    pub tok: Option<Tok>,
}

/// What to print: `[from, to)` of a text.
pub struct Text<'a> {
    pub snap: &'a Snapshot,
    pub from: u64,
    pub to: u64,
    /// The number of the line `from` is on (1 = the first).
    pub line: u64,
    pub lang: Lang,
    /// The coloring's state at `from` (`state_at`).
    pub state: State,
}

/// The pages would be more than `PAGES_MAX`.
#[derive(Debug, PartialEq, Eq)]
pub struct TooMany;

/// Roughly how many pages `len` bytes in `lines` lines take (rows of 90 columns, 60 to a page), to ask before
/// printing a lot, before the paper is known.
pub fn estimate_pages(len: u64, lines: u64) -> u64 {
    lines.max(len / 90).div_ceil(60).max(1)
}

/// Whether text of kind `tok` is printed bold or italic (as the editor draws it).
pub fn bold(tok: Tok) -> bool {
    matches!(tok, Tok::Bold | Tok::Heading | Tok::Error)
}

pub fn italic(tok: Tok) -> bool {
    tok == Tok::Italic
}

/// The coloring's state at `pos` of `snap`, `line_start` being the start of its line.
pub fn state_at(snap: &Snapshot, lang: Lang, line_start: u64, pos: u64) -> State {
    if lang == Lang::Plain {
        return State::START;
    }
    let mut at = if snap.len() <= HL_MAX { 0 } else { line_start };
    let mut st = State::START;
    let mut buf = Vec::new();
    while at < pos {
        buf.clear();
        let end = (at + CHUNK as u64).min(pos);
        snap.read_into(at, end, &mut buf);
        // (whole lines where there are some: the lexers are given text from a line's start, as in the editor)
        let take = if end < pos { memchr::memrchr(b'\n', &buf).map_or(buf.len(), |p| p + 1) } else { buf.len() };
        st = highlight::lex(lang, &buf[..take], st, None);
        at += take as u64;
    }
    st
}

/// The rows of `text` on paper of `shape`. `going` is told how many bytes are done and says whether to go on (None
/// when it says no).
pub fn layout(text: &Text, shape: Shape, going: &mut dyn FnMut(u64) -> bool) -> Result<Option<Vec<Row>>, TooMany> {
    let shape = Shape { cols: shape.cols.max(1), rows: shape.rows.max(1), tab: shape.tab.max(1) };
    let mut b = Builder::new(shape, text.line);
    let own_lines = text.snap.len() > HL_MAX;
    let mut st = text.state;
    let (mut buf, mut spans, mut toks) = (Vec::new(), Vec::new(), Vec::new());
    // the text not read yet starts at `pos`; `buf[i..]` is read but not laid out
    let (mut pos, mut i) = (text.from, 0);
    loop {
        let nl = memchr::memchr(b'\n', &buf[i..]);
        if nl.is_none() && buf.len() - i <= PIECE && pos < text.to {
            if !going(pos - text.from) {
                return Ok(None);
            }
            buf.drain(..i);
            i = 0;
            let end = (pos + CHUNK as u64).min(text.to);
            text.snap.read_into(pos, end, &mut buf);
            pos = end;
            continue;
        }
        if i == buf.len() {
            break;
        }
        let (len, line_end) = match nl {
            Some(n) => (n + 1, true),
            None if buf.len() - i > PIECE => {
                // (a long line in pieces, cut between characters)
                let mut n = PIECE;
                while n > PIECE - 4 && is_continuation(buf[i + n]) {
                    n -= 1;
                }
                (n, false)
            }
            // the text's last line, without a line break after it
            None => (buf.len() - i, true),
        };
        if own_lines && b.fresh {
            st = State::START;
        }
        let piece = &buf[i..i + len];
        st = highlight::lex(text.lang, piece, st, Some(&mut spans));
        // what each byte is: a later span wins where they overlap (as in the editor)
        toks.clear();
        toks.resize(len, None);
        for &(s, e, tok) in &spans {
            for t in &mut toks[(s as usize).min(len)..(e as usize).min(len)] {
                *t = Some(tok);
            }
        }
        b.put(piece, &toks, line_end)?;
        i += len;
    }
    Ok(Some(b.finish()))
}

/// Columns a character takes: two for East Asian wide ones and emoji, none for marks that go with the one before.
pub fn width(c: char) -> usize {
    match c as u32 {
        0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F | 0x20D0..=0x20FF | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F => 0,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Rows made a piece of text at a time. A row ends at a space where it can, as word wrap does in the editor: the
/// characters since the last space wait in `word` until it's known whether they fit in the row; a word longer than
/// a row is cut where the row ends.
struct Builder {
    shape: Shape,
    rows: Vec<Row>,
    row: Row,
    /// Columns used in `row`.
    col: usize,
    /// Columns since the start of the line, `word` included (tab stops count from there).
    line_col: usize,
    word: Vec<(char, usize, Option<Tok>)>,
    word_w: usize,
    /// The number of the line laid out now (or next, while `fresh`: nothing of it is laid out yet).
    line: u64,
    fresh: bool,
    max: usize,
}

impl Builder {
    fn new(shape: Shape, line: u64) -> Builder {
        let max = (PAGES_MAX as usize).saturating_mul(shape.rows);
        Builder { shape, rows: Vec::new(), row: Row::default(), col: 0, line_col: 0, word: Vec::new(), word_w: 0, line, fresh: true, max }
    }

    /// Lays out `piece` (with what each of its bytes is), the end of a line when `line_end`.
    fn put(&mut self, piece: &[u8], toks: &[Option<Tok>], line_end: bool) -> Result<(), TooMany> {
        let mut text = piece;
        if line_end && text.last() == Some(&b'\n') {
            text = &text[..text.len() - 1];
            if text.last() == Some(&b'\r') {
                text = &text[..text.len() - 1];
            }
        }
        if self.fresh {
            self.row.line = Some(self.line);
            self.fresh = false;
        }
        let mut k = 0;
        while k < text.len() {
            let (c, n) = decode_char(&text[k..]);
            let tok = toks[k];
            if c == '\t' {
                let tab = self.shape.tab;
                for _ in 0..tab - self.line_col % tab {
                    self.add(' ', 1, tok)?;
                }
            } else {
                let d = display_char(c);
                self.add(d, width(d), tok)?;
            }
            k += n;
        }
        if line_end {
            self.end_word(true)?;
            self.end_row()?;
            self.line += 1;
            self.line_col = 0;
            self.fresh = true;
        }
        Ok(())
    }

    /// The line's next character.
    fn add(&mut self, c: char, w: usize, tok: Option<Tok>) -> Result<(), TooMany> {
        self.line_col += w;
        if c == ' ' {
            self.end_word(true)?;
            if self.col + 1 > self.shape.cols {
                // (a space where a row ends ends it...)
                return self.end_row();
            }
            if self.col == 0 && self.row.line.is_none() {
                // (...and the rest of a line doesn't start with one)
                return Ok(());
            }
            return self.place(c, w, tok);
        }
        if w == 2 {
            // (East Asian text has no spaces: a row can end after any wide character)
            self.end_word(true)?;
            self.word.push((c, w, tok));
            self.word_w = w;
            return self.end_word(true);
        }
        self.word.push((c, w, tok));
        self.word_w += w;
        if self.word_w > self.shape.cols {
            self.end_word(false)?;
        }
        Ok(())
    }

    /// Puts the word in the row, or in the next one when it doesn't fit (`whole`); one longer than a row is cut where
    /// the row ends.
    fn end_word(&mut self, whole: bool) -> Result<(), TooMany> {
        if self.word.is_empty() {
            return Ok(());
        }
        if whole && self.col > 0 && self.col + self.word_w > self.shape.cols {
            self.end_row()?;
        }
        let word = std::mem::take(&mut self.word);
        for &(c, w, tok) in &word {
            self.place(c, w, tok)?;
        }
        self.word = word;
        self.word.clear();
        self.word_w = 0;
        Ok(())
    }

    fn place(&mut self, c: char, w: usize, tok: Option<Tok>) -> Result<(), TooMany> {
        if self.col + w > self.shape.cols && self.col > 0 {
            self.end_row()?;
        }
        let start = self.row.text.len();
        self.row.text.push(c);
        let end = self.row.text.len();
        match self.row.runs.last_mut() {
            Some(r) if r.tok == tok && r.end == start => r.end = end,
            _ => self.row.runs.push(Run { start, end, col: self.col, tok }),
        }
        self.col += w;
        Ok(())
    }

    fn end_row(&mut self) -> Result<(), TooMany> {
        if self.rows.len() >= self.max {
            return Err(TooMany);
        }
        self.rows.push(std::mem::take(&mut self.row));
        self.col = 0;
        Ok(())
    }

    fn finish(mut self) -> Vec<Row> {
        if self.rows.is_empty() {
            // (nothing to print: one empty line, as a blank page)
            self.row.line = Some(self.line);
            self.rows.push(self.row);
        }
        self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::buffer::Buffer;
    use crate::core::source::Source;
    use std::sync::Arc;

    fn snap(text: &[u8]) -> Snapshot {
        Buffer::from_source(Arc::new(Source::from_vec(text.to_vec())), 0).snapshot()
    }

    fn rows_of(text: &[u8], lang: Lang, shape: Shape) -> Vec<Row> {
        let s = snap(text);
        let t = Text { snap: &s, from: 0, to: s.len(), line: 1, lang, state: State::START };
        layout(&t, shape, &mut |_| true).unwrap().unwrap()
    }

    fn texts(rows: &[Row]) -> Vec<(Option<u64>, &str)> {
        rows.iter().map(|r| (r.line, r.text.as_str())).collect()
    }

    const SHAPE: Shape = Shape { cols: 10, rows: 50, tab: 4 };

    #[test]
    fn long_lines_go_on_in_the_next_rows_tabs_go_to_stops_and_the_invisible_shows() {
        let rows = rows_of(b"ab\tc\r\n0123456789abcdef\n\x01\rx\xFFy\n\n\tz", Lang::Plain, SHAPE);
        assert_eq!(
            texts(&rows),
            [
                (Some(1), "ab  c"),
                (Some(2), "0123456789"),
                (None, "abcdef"),
                (Some(3), "\u{2401}\u{240D}x\u{FFFD}y"),
                (Some(4), ""),
                (Some(5), "    z"),
            ]
        );
        // a tab past a row's end ends it (the rest of the line doesn't start with spaces); tab stops count from the
        // line's start
        let rows = rows_of(b"12345678\tx\tz", Lang::Plain, SHAPE);
        assert_eq!(texts(&rows), [(Some(1), "12345678  "), (None, "x   z")]);
    }

    #[test]
    fn rows_end_at_spaces_and_only_a_word_longer_than_a_row_is_cut() {
        let rows = rows_of(b"the quick brown fox", Lang::Plain, SHAPE);
        assert_eq!(texts(&rows), [(Some(1), "the quick "), (None, "brown fox")]);
        let rows = rows_of(b"a 0123456789012 b", Lang::Plain, SHAPE);
        assert_eq!(texts(&rows), [(Some(1), "a 01234567"), (None, "89012 b")]);
        // a word that fills a row exactly, then a space: the next word starts the next row
        let rows = rows_of(b"0123456789 next", Lang::Plain, SHAPE);
        assert_eq!(texts(&rows), [(Some(1), "0123456789"), (None, "next")]);
    }

    #[test]
    fn wide_characters_take_two_columns_and_a_text_ending_in_a_line_break_has_no_empty_row_after() {
        let rows = rows_of("日本語のテキスト\n".as_bytes(), Lang::Plain, SHAPE);
        assert_eq!(texts(&rows), [(Some(1), "日本語のテ"), (None, "キスト")]);
        assert_eq!(rows[1].runs, [Run { start: 0, end: 9, col: 0, tok: None }]);
        assert_eq!(texts(&rows_of(b"", Lang::Plain, SHAPE)), [(Some(1), "")]);
    }

    #[test]
    fn rows_are_colored_as_the_editor_colors_them() {
        let rows = rows_of(br#"{"name": 12}"#, Lang::Json, Shape { cols: 80, ..SHAPE });
        let r = &rows[0];
        let piece = |run: &Run| (&r.text[run.start..run.end], run.col, run.tok);
        let runs: Vec<_> = r.runs.iter().map(piece).collect();
        assert!(runs.contains(&("\"name\"", 1, Some(Tok::Key))), "{runs:?}");
        assert!(runs.contains(&("12", 9, Some(Tok::Num))), "{runs:?}");
        // the runs cover the row, in order
        assert_eq!(r.runs.first().unwrap().start, 0);
        assert_eq!(r.runs.last().unwrap().end, r.text.len());
        assert!(r.runs.windows(2).all(|w| w[0].end == w[1].start));
    }

    #[test]
    fn a_selection_starts_in_the_state_the_text_before_it_left() {
        let text = b"int x;\n/* a comment\nstill in it\nend */ int y;\n";
        let s = snap(text);
        let from = text.windows(5).position(|w| w == b"still").unwrap() as u64;
        let ls = from;
        let state = state_at(&s, Lang::C, ls, from);
        let t = Text { snap: &s, from, to: s.len(), line: 3, lang: Lang::C, state };
        let rows = layout(&t, Shape { cols: 80, ..SHAPE }, &mut |_| true).unwrap().unwrap();
        assert_eq!(texts(&rows), [(Some(3), "still in it"), (Some(4), "end */ int y;")]);
        assert_eq!(rows[0].runs, [Run { start: 0, end: 11, col: 0, tok: Some(Tok::Comment) }]);
        assert_eq!(rows[1].runs[0].tok, Some(Tok::Comment));
    }

    #[test]
    fn long_texts_are_read_in_chunks_and_long_lines_in_pieces_without_losing_anything() {
        // lines of many lengths, some longer than a piece, some characters of several bytes across the cuts
        let mut text = Vec::new();
        let mut want = Vec::new();
        for n in 0..100usize {
            let len = (n * 7919) % (3 * PIECE);
            let line: String = (0..len).map(|k| if k % 3 == 0 { 'é' } else { (b'a' + (k % 26) as u8) as char }).collect();
            text.extend_from_slice(line.as_bytes());
            text.push(b'\n');
            want.push(line);
        }
        assert!(text.len() > 3 * CHUNK);
        let shape = Shape { cols: 1000, rows: 60, tab: 4 };
        let rows = rows_of(&text, Lang::Plain, shape);
        let mut lines: Vec<String> = Vec::new();
        for r in &rows {
            match r.line {
                Some(l) => {
                    assert_eq!(l as usize, lines.len() + 1);
                    lines.push(r.text.clone());
                }
                None => lines.last_mut().unwrap().push_str(&r.text),
            }
            assert!(r.text.chars().count() <= 1000);
        }
        assert_eq!(lines, want);
    }

    #[test]
    fn too_many_pages_are_refused_and_stopping_stops() {
        let text = "a\n".repeat(PAGES_MAX as usize + 1);
        let s = snap(text.as_bytes());
        let t = Text { snap: &s, from: 0, to: s.len(), line: 1, lang: Lang::Plain, state: State::START };
        let shape = Shape { cols: 10, rows: 1, tab: 4 };
        assert_eq!(layout(&t, shape, &mut |_| true), Err(TooMany));
        assert_eq!(layout(&t, Shape { rows: 2, ..shape }, &mut |_| true).map(|r| r.map(|r| r.len())), Ok(Some(10_001)));
        assert_eq!(layout(&t, shape, &mut |_| false), Ok(None));
        assert_eq!(estimate_pages(0, 0), 1);
        assert_eq!(estimate_pages(800 << 20, 1), 155_345);
        assert_eq!(estimate_pages(5000, 120), 2);
    }
}
