//! Streaming XML tools: pretty-print, minify and check documents of any size in one pass, like `json.rs` (output
//! goes to a writer, e.g. a temp file). Memory holds only the names of the open elements and at most 1 MiB of
//! whitespace waiting to be written or dropped.
//!
//! Formatting never changes text: markup that only whitespace separates from the markup before it goes on its own
//! line (that whitespace is dropped), while text with content and CDATA are written exactly as they are with
//! nothing added next to them, so `<p>Hello <b>world</b>!</p>` stays as it is. Tags are tidied up (`<a  x = "1" />`
//! becomes `<a x="1"/>`); comments, CDATA, processing instructions and the DOCTYPE are copied as they are. Several
//! top-level elements are accepted when formatting, not when checking.

use std::io::{self, Write};

use super::buffer::Snapshot;
use super::job::Ctx;
use super::json::Mode;
use super::source::IndexBuilder;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub offset: u64,
    pub msg: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XmlStats {
    pub elements: u64,
    pub max_depth: usize,
    /// Number of top-level elements (more than one is only accepted when formatting).
    pub roots: u64,
}

/// Where we are in a reference: `&name;`, `&#123;` or `&#x1F;`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ref {
    Start,
    Name,
    Hash,
    Dec,
    X,
    Hex,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    /// Between markup.
    Text,
    /// In a reference, in text (`quote` 0) or in an attribute value.
    Ref { quote: u8, r: Ref },
    /// After `<`.
    Lt,
    /// After `<!`.
    Bang,
    /// Matching the rest of `<!--`, `<![CDATA[` or `<!DOCTYPE`.
    Word { word: &'static [u8], i: u8 },
    /// In a comment, CDATA section or processing instruction, until `end` (`-->`, `]]>`, `?>`); `run` = how many
    /// of the `-`, `]` or `?` before its `>` were just seen. `dtd`: inside the DOCTYPE.
    Body { end: &'static [u8], run: u8, dtd: bool },
    /// In the DOCTYPE: `subset` = inside `[...]`, `quote` = the open quote (0 if none), `lt` = how much of `<!--`
    /// or `<?` was just seen in the subset.
    Doctype { subset: bool, quote: u8, lt: u8 },
    StartName,
    /// In a start tag after the name or an attribute; `space` = whitespace seen since.
    InTag { space: bool },
    /// After `/` in a start tag.
    Slash,
    AttrName,
    /// After an attribute name, before `=`.
    Eq,
    /// After `=`, before the quote.
    Quote,
    Value { quote: u8 },
    EndName,
    /// After the name in an end tag, before `>`.
    EndGt,
}

pub struct Formatter<'w> {
    mode: Mode,
    st: St,
    /// The names of the open elements, one after another.
    names: Vec<u8>,
    /// For each open element: where its name starts in `names` and the offset of its start tag.
    stack: Vec<(usize, u64)>,
    /// The attribute names of the current start tag, each after a 0 byte (to find duplicates).
    attrs: Vec<u8>,
    /// Where the current attribute name starts in `attrs`.
    attr: usize,
    /// The name in the current end tag.
    end: Vec<u8>,
    /// Whitespace since the last markup, held back until we know whether it belongs to text (dropped if markup
    /// follows; a run longer than `MAX_WS` is kept as text).
    ws: Vec<u8>,
    /// Text (other than whitespace) or CDATA was written since the last markup.
    text: bool,
    /// The last markup was a start tag (so an end tag right after it stays next to it: `<a></a>`).
    open: bool,
    /// Something was written (the first markup needs no line break before it).
    any: bool,
    /// The depth of the outermost open element that has text in it: no line breaks are added inside it, so mixed
    /// content like `<p>Hello <b>x</b></p>` stays as it is (usize::MAX: none).
    mixed: usize,
    /// Offsets of the `<` of the current markup, of the current attribute name and of the current `&`.
    mark: u64,
    name_at: u64,
    ref_at: u64,
    indent: Vec<u8>,
    eol: Vec<u8>,
    out: Vec<u8>,
    w: Option<&'w mut dyn Write>,
    idx: Option<&'w mut IndexBuilder>,
    stats: XmlStats,
    pos: u64,
    io_err: Option<io::Error>,
}

const BAD_BANG: &str = "Expected <!--, <![CDATA[ or <!DOCTYPE";
const MAX_WS: usize = 1 << 20;

fn err(offset: u64, msg: impl Into<String>) -> XmlError {
    XmlError { offset, msg: msg.into() }
}

fn describe(b: u8) -> String {
    match b {
        b'"' => "'\"'".into(),
        _ if b.is_ascii_graphic() => format!("\"{}\"", b as char),
        _ => format!("byte 0x{b:02X}"),
    }
}

/// A name for a message, shortened if it is very long.
fn show(name: &[u8]) -> String {
    let mut n = name.len().min(60);
    while n > 0 && n < name.len() && name[n] & 0xC0 == 0x80 {
        n -= 1;
    }
    let dots = if n < name.len() { "..." } else { "" };
    format!("{}{dots}", String::from_utf8_lossy(&name[..n]))
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

/// Non-ASCII bytes count as name characters (letters of other scripts).
fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80
}

fn is_name(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-' || b == b'.'
}

impl<'w> Formatter<'w> {
    /// `indent` is the indentation unit (e.g. two spaces or a tab), `eol` the line ending to use.
    pub fn new(
        mode: Mode,
        indent: &[u8],
        eol: &[u8],
        w: Option<&'w mut dyn Write>,
        idx: Option<&'w mut IndexBuilder>,
    ) -> Self {
        Formatter {
            mode,
            st: St::Text,
            names: Vec::new(),
            stack: Vec::new(),
            attrs: Vec::new(),
            attr: 0,
            end: Vec::new(),
            ws: Vec::new(),
            text: false,
            open: false,
            any: false,
            mixed: usize::MAX,
            mark: 0,
            name_at: 0,
            ref_at: 0,
            indent: indent.to_vec(),
            eol: eol.to_vec(),
            out: Vec::with_capacity(1 << 20),
            w,
            idx,
            stats: XmlStats::default(),
            pos: 0,
            io_err: None,
        }
    }

    fn emit(&mut self, b: &[u8]) {
        if self.mode != Mode::Validate {
            self.out.extend_from_slice(b);
            if self.out.len() >= 1 << 20 {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        if self.out.is_empty() {
            return;
        }
        if let Some(i) = self.idx.as_mut() {
            i.push(&self.out);
        }
        if let Some(w) = self.w.as_mut() {
            if let Err(e) = w.write_all(&self.out) {
                self.io_err.get_or_insert(e);
            }
        }
        self.out.clear();
    }

    fn newline(&mut self, depth: usize) {
        if self.mode == Mode::Pretty {
            let eol = std::mem::take(&mut self.eol);
            self.emit(&eol);
            self.eol = eol;
            let ind = std::mem::take(&mut self.indent);
            for _ in 0..depth {
                self.emit(&ind);
            }
            self.indent = ind;
        }
    }

    /// The name of the innermost open element.
    fn top(&self) -> &[u8] {
        self.stack.last().map_or(&[], |&(s, _)| &self.names[s..])
    }

    /// Before a tag, comment, processing instruction or DOCTYPE at `depth`: drops the whitespace before it and
    /// (in Pretty mode) starts a new line, unless text comes right before it or `glue` is set.
    fn markup(&mut self, depth: usize, glue: bool) {
        if !self.text {
            self.ws.clear();
            if self.any && !glue && self.stack.len() < self.mixed {
                self.newline(depth);
            }
        }
        self.text = false;
        self.open = false;
        self.any = true;
    }

    /// Text that isn't only whitespace, CDATA or a run of whitespace too long to hold back: writes the whitespace
    /// held back before it.
    fn content(&mut self, at: u64) -> Result<(), XmlError> {
        if self.stack.is_empty() {
            return Err(err(at, "Text outside the root element"));
        }
        self.mixed = self.mixed.min(self.stack.len());
        if !self.text {
            self.text = true;
            let ws = std::mem::take(&mut self.ws);
            self.emit(&ws);
            self.ws = ws;
            self.ws.clear();
        }
        Ok(())
    }

    /// `<` and the first letter of a start tag.
    fn start_tag(&mut self, b: u8) -> Result<(), XmlError> {
        if self.stack.is_empty() {
            self.stats.roots += 1;
            if self.stats.roots > 1 && self.mode == Mode::Validate {
                return Err(err(self.mark, "Only one top-level element is allowed"));
            }
        }
        self.markup(self.stack.len(), false);
        self.emit(&[b'<', b]);
        self.stack.push((self.names.len(), self.mark));
        self.names.push(b);
        self.attrs.clear();
        self.stats.elements += 1;
        self.stats.max_depth = self.stats.max_depth.max(self.stack.len());
        self.st = St::StartName;
        Ok(())
    }

    /// The name of an attribute is complete: it must not repeat an earlier one in the tag.
    fn attr_name(&self) -> Result<(), XmlError> {
        let (seen, name) = self.attrs.split_at(self.attr);
        if seen.split(|&c| c == 0).any(|n| n == name) {
            let msg = format!("Attribute {} appears twice in <{}>", show(name), show(self.top()));
            return Err(err(self.name_at, msg));
        }
        Ok(())
    }

    /// The name of an end tag is complete: it must be the innermost open element's.
    fn end_name(&self) -> Result<(), XmlError> {
        if !self.end.is_empty() && self.top() == &self.end[..] {
            return Ok(());
        }
        let found = show(&self.end);
        let msg = if self.end.is_empty() {
            "Expected a tag name after \"</\"".to_string()
        } else if self.stack.is_empty() {
            format!("</{found}> has no matching start tag")
        } else {
            format!("Expected </{}> but found </{found}>", show(self.top()))
        };
        Err(err(self.mark, msg))
    }

    fn pop(&mut self) {
        let (start, _) = self.stack.pop().unwrap();
        self.names.truncate(start);
        if self.stack.len() < self.mixed {
            self.mixed = usize::MAX;
        }
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<(), XmlError> {
        let base = self.pos;
        let mut i = 0usize;
        while i < data.len() {
            let at = base + i as u64;
            let b = data[i];
            match self.st {
                St::Text => {
                    let rest = &data[i..];
                    let j = memchr::memchr2(b'<', b'&', rest).unwrap_or(rest.len());
                    let mut s = &rest[..j];
                    if !self.text {
                        // Whitespace is held back until we know what follows it (outside the root element it is
                        // always dropped).
                        let k = s.iter().position(|&c| !is_space(c)).unwrap_or(s.len());
                        let hold = self.mode != Mode::Validate && !self.stack.is_empty();
                        if k < s.len() || (hold && self.ws.len() + k > MAX_WS) {
                            self.content(at + k as u64)?;
                        } else {
                            if hold {
                                self.ws.extend_from_slice(s);
                            }
                            s = &[];
                        }
                    }
                    self.emit(s);
                    i += j;
                    if i < data.len() {
                        let at = base + i as u64;
                        if data[i] == b'&' {
                            self.content(at)?;
                            self.ref_at = at;
                            self.emit(b"&");
                            self.st = St::Ref { quote: 0, r: Ref::Start };
                        } else {
                            self.mark = at;
                            self.st = St::Lt;
                        }
                        i += 1;
                    }
                    continue;
                }
                St::Value { quote } => {
                    let rest = &data[i..];
                    let j = memchr::memchr3(quote, b'<', b'&', rest).unwrap_or(rest.len());
                    self.emit(&rest[..j]);
                    i += j;
                    if i < data.len() {
                        let at = base + i as u64;
                        match data[i] {
                            b'<' => return Err(err(at, "Unexpected \"<\" inside an attribute value")),
                            b'&' => {
                                self.ref_at = at;
                                self.emit(b"&");
                                self.st = St::Ref { quote, r: Ref::Start };
                            }
                            _ => {
                                self.emit(&[quote]);
                                self.st = St::InTag { space: false };
                            }
                        }
                        i += 1;
                    }
                    continue;
                }
                St::Body { end, run, dtd } => {
                    // Ends at a `>` right after `--`, `]]` or `?` (`end` without its `>`); `run` carries how many of
                    // those ended the previous chunk.
                    let rest = &data[i..];
                    let (c, need) = (end[0], end.len() - 1);
                    let j = memchr::memchr(b'>', rest).unwrap_or(rest.len());
                    let tail = rest[..j].iter().rev().take_while(|&&x| x == c).count();
                    let run = (if tail == j { run as usize + tail } else { tail }).min(need);
                    if j == rest.len() {
                        self.emit(rest);
                        self.st = St::Body { end, run: run as u8, dtd };
                        i = data.len();
                        continue;
                    }
                    self.emit(&rest[..=j]);
                    i += j + 1;
                    self.st = if run < need {
                        St::Body { end, run: 0, dtd }
                    } else if dtd {
                        St::Doctype { subset: true, quote: 0, lt: 0 }
                    } else {
                        St::Text
                    };
                    continue;
                }
                St::StartName | St::AttrName | St::EndName => {
                    let rest = &data[i..];
                    let n = rest.iter().position(|&c| !is_name(c)).unwrap_or(rest.len());
                    let name = &rest[..n];
                    match self.st {
                        St::StartName => self.names.extend_from_slice(name),
                        St::AttrName => self.attrs.extend_from_slice(name),
                        _ => self.end.extend_from_slice(name),
                    }
                    if self.st != St::EndName {
                        self.emit(name);
                    }
                    i += n;
                    if i < data.len() {
                        // the name ends here: look at this byte again in the next state
                        self.st = match self.st {
                            St::StartName => St::InTag { space: false },
                            St::AttrName => {
                                self.attr_name()?;
                                St::Eq
                            }
                            _ => {
                                self.end_name()?;
                                St::EndGt
                            }
                        };
                    }
                    continue;
                }
                St::Ref { quote, r } => {
                    let next = match (r, b) {
                        (Ref::Start, b'#') => Some(Ref::Hash),
                        (Ref::Start, _) if is_name_start(b) => Some(Ref::Name),
                        (Ref::Name, _) if is_name(b) => Some(Ref::Name),
                        (Ref::Hash, b'x') => Some(Ref::X),
                        (Ref::Hash | Ref::Dec, b'0'..=b'9') => Some(Ref::Dec),
                        (Ref::X | Ref::Hex, _) if b.is_ascii_hexdigit() => Some(Ref::Hex),
                        (Ref::Name | Ref::Dec | Ref::Hex, b';') => None,
                        (Ref::Start | Ref::Name, _) => return Err(err(self.ref_at, "\"&\" must be written as &amp;")),
                        _ => return Err(err(self.ref_at, "Invalid character code (use &#169; or &#xA9;)")),
                    };
                    self.emit(&[b]);
                    self.st = match next {
                        Some(r) => St::Ref { quote, r },
                        None if quote == 0 => St::Text,
                        None => St::Value { quote },
                    };
                }
                St::Lt => match b {
                    b'/' => {
                        self.end.clear();
                        self.st = St::EndName;
                    }
                    b'?' => {
                        self.markup(self.stack.len(), false);
                        self.emit(b"<?");
                        self.st = St::Body { end: b"?>", run: 0, dtd: false };
                    }
                    b'!' => self.st = St::Bang,
                    _ if is_name_start(b) => self.start_tag(b)?,
                    _ => return Err(err(self.mark, "\"<\" must be written as &lt;")),
                },
                St::Bang => {
                    let word: &'static [u8] = match b {
                        b'-' => b"<!--",
                        b'[' => b"<![CDATA[",
                        b'D' => b"<!DOCTYPE",
                        _ => return Err(err(self.mark, BAD_BANG)),
                    };
                    self.st = St::Word { word, i: 3 };
                }
                St::Word { word, i: k } => {
                    if b != word[k as usize] {
                        return Err(err(self.mark, BAD_BANG));
                    }
                    if (k as usize) + 1 < word.len() {
                        self.st = St::Word { word, i: k + 1 };
                    } else if word == b"<![CDATA[" {
                        self.content(self.mark)?;
                        self.emit(word);
                        self.st = St::Body { end: b"]]>", run: 0, dtd: false };
                    } else if word == b"<!--" {
                        self.markup(self.stack.len(), false);
                        self.emit(word);
                        self.st = St::Body { end: b"-->", run: 0, dtd: false };
                    } else {
                        if self.stats.roots > 0 {
                            return Err(err(self.mark, "The DOCTYPE must come before the root element"));
                        }
                        self.markup(0, false);
                        self.emit(word);
                        self.st = St::Doctype { subset: false, quote: 0, lt: 0 };
                    }
                }
                St::Doctype { subset, quote, lt } => {
                    // Copied as it is; quotes, `[...]` and the comments and PIs in it only matter to find its end.
                    self.emit(&[b]);
                    let dt = |subset, quote, lt| St::Doctype { subset, quote, lt };
                    self.st = match (b, lt) {
                        _ if quote != 0 => dt(subset, if b == quote { 0 } else { quote }, 0),
                        (b'"' | b'\'', _) => dt(subset, b, 0),
                        (b'[', _) => dt(true, 0, 0),
                        (b']', _) => dt(false, 0, 0),
                        (b'>', _) if !subset => St::Text,
                        (b'<', _) if subset => dt(true, 0, 1),
                        (b'!', 1) => dt(true, 0, 2),
                        (b'-', 2) => dt(true, 0, 3),
                        (b'-', 3) => St::Body { end: b"-->", run: 0, dtd: true },
                        (b'?', 1) => St::Body { end: b"?>", run: 0, dtd: true },
                        _ => dt(subset, 0, 0),
                    };
                }
                St::InTag { space } => match b {
                    _ if is_space(b) => self.st = St::InTag { space: true },
                    b'>' => {
                        self.emit(b">");
                        self.open = true;
                        self.st = St::Text;
                    }
                    b'/' => self.st = St::Slash,
                    _ if is_name_start(b) && space => {
                        self.emit(&[b' ', b]);
                        self.attrs.push(0);
                        self.attr = self.attrs.len();
                        self.attrs.push(b);
                        self.name_at = at;
                        self.st = St::AttrName;
                    }
                    _ if is_name_start(b) => return Err(err(at, "Expected a space between attributes")),
                    _ => return Err(err(at, format!("Unexpected {} in <{}>", describe(b), show(self.top())))),
                },
                St::Slash => {
                    if b != b'>' {
                        return Err(err(at, "Expected \">\" after \"/\""));
                    }
                    self.emit(b"/>");
                    self.pop();
                    self.st = St::Text;
                }
                St::Eq => match b {
                    _ if is_space(b) => {}
                    b'=' => self.st = St::Quote,
                    _ => return Err(err(at, format!("Expected \"=\" after {}", show(&self.attrs[self.attr..])))),
                },
                St::Quote => match b {
                    _ if is_space(b) => {}
                    b'"' | b'\'' => {
                        self.emit(&[b'=', b]);
                        self.st = St::Value { quote: b };
                    }
                    _ => {
                        let msg = format!("The value of {} must be in quotes", show(&self.attrs[self.attr..]));
                        return Err(err(at, msg));
                    }
                },
                St::EndGt => match b {
                    _ if is_space(b) => {}
                    b'>' => {
                        let glue = self.open;
                        self.markup(self.stack.len() - 1, glue);
                        let name = std::mem::take(&mut self.end);
                        self.emit(b"</");
                        self.emit(&name);
                        self.emit(b">");
                        self.end = name;
                        self.pop();
                        self.st = St::Text;
                    }
                    _ => return Err(err(at, format!("Unexpected {} in </{}>", describe(b), show(&self.end)))),
                },
            }
            i += 1;
        }
        self.pos += data.len() as u64;
        if let Some(e) = self.io_err.take() {
            return Err(err(self.pos, format!("Couldn't write the result: {e}")));
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<XmlStats, XmlError> {
        let msg = match self.st {
            St::Text | St::Ref { quote: 0, .. } => None,
            St::Doctype { .. } | St::Body { dtd: true, .. } => Some("The DOCTYPE is never closed"),
            St::Body { end: b"-->", .. } => Some("The comment is never closed"),
            St::Body { end: b"]]>", .. } => Some("The CDATA section is never closed"),
            St::Body { .. } => Some("The <?...?> tag is never closed"),
            _ => Some("The file ends inside a tag"),
        };
        if let Some(msg) = msg {
            return Err(err(self.mark, msg));
        }
        if let Some(&(_, at)) = self.stack.last() {
            return Err(err(at, format!("<{}> is never closed", show(self.top()))));
        }
        if self.stats.roots == 0 {
            return Err(err(self.pos, "There is no XML element here"));
        }
        if self.mode == Mode::Pretty {
            let eol = self.eol.clone();
            self.emit(&eol);
        }
        self.flush();
        if let Some(e) = self.io_err.take() {
            return Err(err(self.pos, format!("Couldn't write the result: {e}")));
        }
        Ok(self.stats)
    }
}

/// Runs `mode` over the whole snapshot.
pub fn run<'w>(
    snap: &Snapshot,
    mode: Mode,
    indent: &[u8],
    eol: &[u8],
    w: Option<&'w mut dyn Write>,
    idx: Option<&'w mut IndexBuilder>,
    ctx: &Ctx,
) -> Result<XmlStats, XmlError> {
    let mut f = Formatter::new(mode, indent, eol, w, idx);
    let mut pos = 0u64;
    let mut res = Ok(());
    while pos < snap.len() && res.is_ok() {
        if ctx.cancelled() {
            return Err(err(pos, "Cancelled"));
        }
        let end = (pos + (8 << 20)).min(snap.len());
        snap.chunks(pos, end, &mut |c| {
            res = f.feed(c);
            res.is_ok()
        });
        pos = end;
        ctx.set(pos);
    }
    res?;
    f.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `mode` over `input` fed in pieces of `step` bytes.
    fn feed(mode: Mode, input: &str, step: usize) -> Result<(String, XmlStats), XmlError> {
        let mut out = Vec::new();
        let stats = {
            let mut f = Formatter::new(mode, b"  ", b"\n", Some(&mut out), None);
            for c in input.as_bytes().chunks(step) {
                f.feed(c)?;
            }
            f.finish()?
        };
        Ok((String::from_utf8(out).unwrap(), stats))
    }

    /// Runs `mode` over `input` in one piece and byte by byte, which must give the same result.
    fn fmt(mode: Mode, input: &str) -> Result<String, XmlError> {
        let whole = feed(mode, input, input.len().max(1));
        assert_eq!(whole, feed(mode, input, 1), "{input}");
        whole.map(|(s, _)| s)
    }

    const DOC: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE config [\n  <!ENTITY co \"Acme ]> Inc\">\n  <!-- it's [ok] > -->\n]>\n",
        "<!-- settings -->\n",
        "<config  version = \"2\"\n        mode='a > b \"c\" /'\n  note=\"two\n lines\" >\n",
        "\t<item id=\"1\"/><item id=\"2\" />\n",
        "  <name>Slate &amp; co &#169; &#x1F600;</name>\n",
        "  <empty>  \n  </empty>\n",
        "  <script><![CDATA[if (a < b && c) x = \"]]\";]]></script>\n",
        "  <ns:list xmlns:ns=\"urn:x\"><ns:i-1.x_y/><élément/>\n<!-- a <b> c --></ns:list>\n",
        "</config>\n",
    );

    #[test]
    fn pretty_and_minify() {
        let pretty = fmt(Mode::Pretty, DOC).unwrap();
        assert_eq!(
            pretty,
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE config [\n  <!ENTITY co \"Acme ]> Inc\">\n  <!-- it's [ok] > -->\n]>\n",
                "<!-- settings -->\n",
                "<config version=\"2\" mode='a > b \"c\" /' note=\"two\n lines\">\n",
                "  <item id=\"1\"/>\n  <item id=\"2\"/>\n",
                "  <name>Slate &amp; co &#169; &#x1F600;</name>\n",
                "  <empty></empty>\n",
                "  <script><![CDATA[if (a < b && c) x = \"]]\";]]></script>\n",
                "  <ns:list xmlns:ns=\"urn:x\">\n    <ns:i-1.x_y/>\n    <élément/>\n",
                "    <!-- a <b> c -->\n  </ns:list>\n",
                "</config>\n",
            )
        );
        let min = fmt(Mode::Minify, DOC).unwrap();
        assert_eq!(
            min,
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
                "<!DOCTYPE config [\n  <!ENTITY co \"Acme ]> Inc\">\n  <!-- it's [ok] > -->\n]>",
                "<!-- settings --><config version=\"2\" mode='a > b \"c\" /' note=\"two\n lines\">",
                "<item id=\"1\"/><item id=\"2\"/><name>Slate &amp; co &#169; &#x1F600;</name><empty></empty>",
                "<script><![CDATA[if (a < b && c) x = \"]]\";]]></script>",
                "<ns:list xmlns:ns=\"urn:x\"><ns:i-1.x_y/><élément/><!-- a <b> c --></ns:list></config>",
            )
        );
        assert_eq!(fmt(Mode::Pretty, &pretty).unwrap(), pretty);
        assert_eq!(fmt(Mode::Pretty, &min).unwrap(), pretty);
        assert_eq!(fmt(Mode::Minify, &pretty).unwrap(), min);
        assert_eq!(fmt(Mode::Validate, DOC).unwrap(), "");
        let stats = feed(Mode::Validate, DOC, 1).unwrap().1;
        assert_eq!(stats, XmlStats { elements: 9, max_depth: 3, roots: 1 });
    }

    #[test]
    fn text_is_kept_as_it_is() {
        let pretty = |s: &str| {
            let p = fmt(Mode::Pretty, s).unwrap();
            assert_eq!(fmt(Mode::Pretty, &p).unwrap(), p, "{s}");
            assert_eq!(fmt(Mode::Pretty, &fmt(Mode::Minify, s).unwrap()).unwrap(), p, "{s}");
            p
        };
        assert_eq!(pretty("<p>Hello <b>world</b>!</p>"), "<p>Hello <b>world</b>!</p>\n");
        // once an element has text, nothing is added inside it
        assert_eq!(pretty("<p>Hello <b>x</b></p>"), "<p>Hello <b>x</b></p>\n");
        assert_eq!(pretty("<d><p>Hi <b>a</b><i>b</i></p><q/></d>"), "<d>\n  <p>Hi <b>a</b><i>b</i></p>\n  <q/>\n</d>\n");
        assert_eq!(
            pretty("<doc><p>Hello <b>world</b>!</p>  <p> x </p></doc>"),
            "<doc>\n  <p>Hello <b>world</b>!</p>\n  <p> x </p>\n</doc>\n"
        );
        assert_eq!(pretty("<a>\n  text\n</a>"), "<a>\n  text\n</a>\n");
        assert_eq!(pretty("<a>x <!--c--> y<?pi?>&lt;</a>"), "<a>x <!--c--> y<?pi?>&lt;</a>\n");
        assert_eq!(pretty("<a>\n <![CDATA[ ]]>\n</a>"), "<a>\n <![CDATA[ ]]>\n</a>\n");
        assert_eq!(pretty("<a  x = '1'\n/>"), "<a x='1'/>\n");
        assert_eq!(pretty("<a><b>\n</b ></a\n>"), "<a>\n  <b></b>\n</a>\n");
        assert_eq!(fmt(Mode::Minify, "<a> <b> x </b> </a>\n").unwrap(), "<a><b> x </b></a>");
        // whitespace too long to hold back is kept as text (outside the root element it is still dropped)
        let ws = " ".repeat(MAX_WS + 1);
        assert_eq!(fmt(Mode::Pretty, &format!("{ws}<a>{ws}</a>{ws}")).unwrap(), format!("<a>{ws}</a>\n"));
    }

    #[test]
    fn several_top_level_elements() {
        assert_eq!(fmt(Mode::Pretty, "<a/> <b>x</b>\n<!-- c --><c/>").unwrap(), "<a/>\n<b>x</b>\n<!-- c -->\n<c/>\n");
        let (min, stats) = feed(Mode::Minify, "<a/> <b><c/></b>", 1).unwrap();
        assert_eq!((min.as_str(), stats), ("<a/><b><c/></b>", XmlStats { elements: 3, max_depth: 2, roots: 2 }));
        let e = fmt(Mode::Validate, "<a/> <b/>").unwrap_err();
        assert_eq!((e.offset, e.msg.as_str()), (5, "Only one top-level element is allowed"));
    }

    #[test]
    fn errors_point_at_the_problem() {
        for (src, at, msg) in [
            ("<items><item></items>", 13, "Expected </item> but found </items>"),
            ("<a/></a>", 4, "</a> has no matching start tag"),
            ("<config><item/>", 0, "<config> is never closed"),
            ("<a>\n  <b>x &amp", 6, "<b> is never closed"),
            ("<a x=\"1<2\"/>", 7, "Unexpected \"<\" inside an attribute value"),
            ("<a>AT&T</a>", 5, "\"&\" must be written as &amp;"),
            ("<a>&amp</a>", 3, "\"&\" must be written as &amp;"),
            ("<a>& b</a>", 3, "\"&\" must be written as &amp;"),
            ("<a href=\"?a=1&b=2\"/>", 13, "\"&\" must be written as &amp;"),
            ("<a>&#12a;</a>", 3, "Invalid character code (use &#169; or &#xA9;)"),
            ("<a x='&#x;'/>", 6, "Invalid character code (use &#169; or &#xA9;)"),
            ("<a>&#X1;</a>", 3, "Invalid character code (use &#169; or &#xA9;)"),
            ("<a x/>", 4, "Expected \"=\" after x"),
            ("<a x=1/>", 5, "The value of x must be in quotes"),
            ("<a x='1' y=\"2\" x=\"3\"/>", 15, "Attribute x appears twice in <a>"),
            ("<a x=\"1\"y=\"2\"/>", 8, "Expected a space between attributes"),
            ("<a b=\"1\" =2>", 9, "Unexpected \"=\" in <a>"),
            ("<a/ >", 3, "Expected \">\" after \"/\""),
            ("<a></a b>", 7, "Unexpected \"b\" in </a>"),
            ("<a></>", 3, "Expected a tag name after \"</\""),
            ("<a>1 < 2</a>", 5, "\"<\" must be written as &lt;"),
            ("<a><!foo></a>", 3, "Expected <!--, <![CDATA[ or <!DOCTYPE"),
            ("<a/>x", 4, "Text outside the root element"),
            ("  hi <a/>", 2, "Text outside the root element"),
            ("<a/>&amp;", 4, "Text outside the root element"),
            ("<![CDATA[x]]><a/>", 0, "Text outside the root element"),
            ("<a/><!DOCTYPE a>", 4, "The DOCTYPE must come before the root element"),
            ("<a><!-- x --</a>", 3, "The comment is never closed"),
            ("<a><![CDATA[x]]</a>", 3, "The CDATA section is never closed"),
            ("<?xml version=\"1.0\"", 0, "The <?...?> tag is never closed"),
            ("<!DOCTYPE a [ <!-- ]> -->", 0, "The DOCTYPE is never closed"),
            ("<a><b x=\"1>\"", 3, "The file ends inside a tag"),
            ("", 0, "There is no XML element here"),
            ("<!-- only -->", 13, "There is no XML element here"),
        ] {
            let e = fmt(Mode::Validate, src).unwrap_err();
            assert_eq!((e.offset, e.msg.as_str()), (at, msg), "{src}");
            for mode in [Mode::Pretty, Mode::Minify] {
                assert_eq!(fmt(mode, src).unwrap_err(), e, "{src}");
            }
        }
        assert!(fmt(Mode::Validate, "<a b=\"&amp;&#60;&#x3C;&ns:x-1;\">&lt;&#0123;&#xaF;</a>").is_ok());
    }

    #[test]
    fn big_document() {
        let mut src = String::from("<?xml version=\"1.0\"?>\n<root>\n");
        for i in 0..25_000 {
            src += &format!("  <item id=\"{i}\" kind='a &amp; \"b\"'>\n    <name>Item {i} &lt; x</name>\n");
            src += "    <flag/>\n  </item>\n";
        }
        src += "</root>\n";
        let t = std::time::Instant::now();
        let (pretty, stats) = feed(Mode::Pretty, &src, 1 << 16).unwrap();
        assert_eq!(pretty, src);
        assert_eq!(stats, XmlStats { elements: 75_001, max_depth: 3, roots: 1 });
        let (min, _) = feed(Mode::Minify, &src, 7).unwrap();
        assert_eq!(feed(Mode::Pretty, &min, 1 << 20).unwrap().0, src);
        assert!(t.elapsed().as_secs_f64() < 1.0, "{:?}", t.elapsed());
    }
}
