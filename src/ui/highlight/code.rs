//! Programming and scripting languages: one configurable lexer (comments, strings, numbers, keywords, and a few
//! extras per language such as Rust raw strings or PowerShell here-strings), and CSS.

use super::{Lang, Out, State, Tok, at, find, line_end, scan_str};

// What `State::kind` means here.
const LINE_COMMENT: u8 = 1;
/// `a`: nesting depth.
const BLOCK_COMMENT: u8 = 2;
/// `a`: the quote, `b`: 1 if an escape is pending.
const STR: u8 = 3;
/// `"""…"""`; `a`: the quote, `b`: escape pending.
const TRIPLE: u8 = 4;
/// Rust `r#"…"#`; `a`: the number of `#`.
const RAW: u8 = 5;
/// C# `@"…"` (`""` is a quote).
const VERBATIM: u8 = 6;
/// PowerShell `@"…"@`; `a`: the quote.
const HERE: u8 = 7;
/// Lua `[[…]]` / `--[[…]]`; `a`: the number of `=`, `b`: 1 for a comment.
const LONG: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Plain,
    Rust,
    CSharp,
    Lua,
    Sql,
    PowerShell,
    Batch,
    Shell,
    Ruby,
    Php,
}

pub(super) struct Syntax {
    flavor: Flavor,
    line: &'static [&'static [u8]],
    block: Option<(&'static [u8], &'static [u8])>,
    nested: bool,
    /// String quotes; strings with `multi` quotes may span lines, `raw` quotes have no escapes.
    quotes: &'static [u8],
    multi: &'static [u8],
    raw: &'static [u8],
    /// Quotes that can be tripled (`"""…"""`) into a multi-line string.
    triple: &'static [u8],
    esc: u8,
    nocase: bool,
    // sorted, lowercase when `nocase`
    kw: &'static [&'static str],
    ctl: &'static [&'static str],
    types: &'static [&'static str],
    lits: &'static [&'static str],
    vars: &'static [&'static str],
    /// Characters that start a variable (`$x`, `@x`).
    sigils: &'static [u8],
    dollar_ident: bool,
    dash_ident: bool,
    /// `#include`-style lines.
    preproc: bool,
    /// `@Decorator` / `@Annotation`.
    decorators: bool,
    /// Capitalized names are types (`String`, `HashMap`).
    cap_types: bool,
    /// A name followed by `(` is a function.
    calls: bool,
}

const BASE: Syntax = Syntax {
    flavor: Flavor::Plain,
    line: &[b"//"],
    block: Some((b"/*", b"*/")),
    nested: false,
    quotes: b"\"'",
    multi: b"",
    raw: b"",
    triple: b"",
    esc: b'\\',
    nocase: false,
    kw: &[],
    ctl: &[],
    types: &[],
    lits: &[],
    vars: &[],
    sigils: b"",
    dollar_ident: false,
    dash_ident: false,
    preproc: false,
    decorators: false,
    cap_types: false,
    calls: true,
};

static PLAIN: Syntax = BASE;

static CPP: Syntax = Syntax {
    kw: &[
        "alignas", "alignof", "auto", "class", "concept", "const", "const_cast", "consteval", "constexpr", "constinit",
        "decltype", "delete", "dynamic_cast", "enum", "explicit", "export", "extern", "final", "friend", "inline",
        "mutable", "namespace", "new", "noexcept", "operator", "override", "private", "protected", "public", "register",
        "reinterpret_cast", "requires", "restrict", "signed", "sizeof", "static", "static_assert", "static_cast",
        "struct", "template", "this", "thread_local", "typedef", "typeid", "typename", "union", "unsigned", "using",
        "virtual", "volatile",
    ],
    ctl: &[
        "break", "case", "catch", "co_await", "co_return", "co_yield", "continue", "default", "do", "else", "for",
        "goto", "if", "return", "switch", "throw", "try", "while",
    ],
    types: &[
        "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int", "int16_t", "int32_t", "int64_t",
        "int8_t", "long", "ptrdiff_t", "short", "size_t", "ssize_t", "uint16_t", "uint32_t", "uint64_t", "uint8_t",
        "uintptr_t", "void", "wchar_t",
    ],
    lits: &["NULL", "false", "nullptr", "true"],
    preproc: true,
    ..BASE
};

static CSHARP: Syntax = Syntax {
    flavor: Flavor::CSharp,
    kw: &[
        "abstract", "add", "as", "async", "base", "checked", "class", "const", "delegate", "enum", "event", "explicit",
        "extern", "file", "fixed", "get", "global", "implicit", "in", "init", "interface", "internal", "is", "lock",
        "nameof", "namespace", "new", "operator", "out", "override", "params", "partial", "private", "protected",
        "public", "readonly", "record", "ref", "remove", "required", "scoped", "sealed", "set", "sizeof", "stackalloc",
        "static", "struct", "this", "typeof", "unchecked", "unsafe", "using", "value", "var", "virtual", "volatile",
        "where", "with",
    ],
    ctl: &[
        "await", "break", "case", "catch", "continue", "default", "do", "else", "finally", "for", "foreach", "goto",
        "if", "return", "switch", "throw", "try", "when", "while", "yield",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "nint", "nuint", "object",
        "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
    ],
    lits: &["false", "null", "true"],
    triple: b"\"",
    preproc: true,
    cap_types: true,
    ..BASE
};

static JAVA: Syntax = Syntax {
    kw: &[
        "abstract", "assert", "class", "const", "enum", "extends", "final", "implements", "instanceof", "interface",
        "native", "new", "package", "permits", "private", "protected", "public", "record", "sealed", "static",
        "strictfp", "super", "synchronized", "this", "throws", "transient", "var", "volatile",
    ],
    ctl: &[
        "break", "case", "catch", "continue", "default", "do", "else", "finally", "for", "if", "import", "return",
        "switch", "throw", "try", "while", "yield",
    ],
    types: &["boolean", "byte", "char", "double", "float", "int", "long", "short", "void"],
    lits: &["false", "null", "true"],
    triple: b"\"",
    decorators: true,
    cap_types: true,
    ..BASE
};

static KOTLIN: Syntax = Syntax {
    kw: &[
        "abstract", "actual", "annotation", "as", "by", "class", "companion", "const", "constructor", "crossinline",
        "data", "enum", "expect", "external", "final", "fun", "in", "infix", "init", "inline", "inner", "interface",
        "internal", "is", "lateinit", "noinline", "object", "open", "operator", "out", "override", "package",
        "private", "protected", "public", "reified", "sealed", "super", "suspend", "tailrec", "this", "typealias",
        "val", "var", "vararg",
    ],
    ctl: &["break", "catch", "continue", "do", "else", "finally", "for", "if", "import", "return", "throw", "try", "when", "while"],
    lits: &["false", "null", "true"],
    triple: b"\"",
    nested: true,
    decorators: true,
    cap_types: true,
    ..BASE
};

static SWIFT: Syntax = Syntax {
    kw: &[
        "Self", "actor", "any", "associatedtype", "async", "class", "convenience", "deinit", "dynamic", "enum",
        "extension", "fileprivate", "final", "func", "indirect", "init", "inout", "internal", "lazy", "let",
        "mutating", "nonmutating", "open", "operator", "optional", "override", "private", "protocol", "public",
        "required", "rethrows", "self", "some", "static", "struct", "subscript", "super", "throws", "typealias",
        "unowned", "var", "weak",
    ],
    ctl: &[
        "await", "break", "case", "catch", "continue", "default", "defer", "do", "else", "fallthrough", "for", "guard",
        "if", "import", "in", "repeat", "return", "switch", "throw", "try", "where", "while",
    ],
    lits: &["false", "nil", "true"],
    quotes: b"\"",
    triple: b"\"",
    nested: true,
    decorators: true,
    cap_types: true,
    ..BASE
};

static GO: Syntax = Syntax {
    kw: &["chan", "const", "func", "interface", "map", "package", "struct", "type", "var"],
    ctl: &[
        "break", "case", "continue", "default", "defer", "else", "fallthrough", "for", "go", "goto", "if", "import",
        "range", "return", "select", "switch",
    ],
    types: &[
        "any", "bool", "byte", "complex128", "complex64", "error", "float32", "float64", "int", "int16", "int32",
        "int64", "int8", "rune", "string", "uint", "uint16", "uint32", "uint64", "uint8", "uintptr",
    ],
    lits: &["false", "iota", "nil", "true"],
    quotes: b"\"'`",
    multi: b"`",
    raw: b"`",
    ..BASE
};

static RUST: Syntax = Syntax {
    flavor: Flavor::Rust,
    kw: &[
        "Self", "as", "async", "const", "crate", "dyn", "enum", "extern", "fn", "impl", "let", "mod", "move", "mut",
        "pub", "ref", "self", "static", "struct", "super", "trait", "type", "union", "unsafe", "use", "where",
    ],
    ctl: &["await", "break", "continue", "else", "for", "if", "in", "loop", "match", "return", "while", "yield"],
    types: &[
        "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16", "u32", "u64",
        "u8", "usize",
    ],
    lits: &["false", "true"],
    quotes: b"\"",
    multi: b"\"",
    nested: true,
    cap_types: true,
    ..BASE
};

static PYTHON: Syntax = Syntax {
    line: &[b"#"],
    block: None,
    kw: &["and", "as", "async", "class", "def", "del", "global", "in", "is", "lambda", "nonlocal", "not", "or"],
    ctl: &[
        "assert", "await", "break", "continue", "elif", "else", "except", "finally", "for", "from", "if", "import",
        "pass", "raise", "return", "try", "while", "with", "yield",
    ],
    types: &[
        "bool", "bytearray", "bytes", "complex", "dict", "float", "frozenset", "int", "list", "object", "set", "str",
        "tuple", "type",
    ],
    lits: &["False", "None", "True"],
    vars: &["cls", "self"],
    triple: b"\"'",
    decorators: true,
    cap_types: true,
    ..BASE
};

static JS: Syntax = Syntax {
    kw: &[
        "async", "class", "const", "debugger", "delete", "extends", "function", "in", "instanceof", "let", "new", "of",
        "static", "super", "this", "typeof", "var", "void",
    ],
    ctl: &[
        "await", "break", "case", "catch", "continue", "default", "do", "else", "export", "finally", "for", "from",
        "if", "import", "return", "switch", "throw", "try", "while", "with", "yield",
    ],
    lits: &["Infinity", "NaN", "false", "null", "true", "undefined"],
    quotes: b"\"'`",
    multi: b"`",
    dollar_ident: true,
    decorators: true,
    cap_types: true,
    ..BASE
};

static TS: Syntax = Syntax {
    kw: &[
        "abstract", "accessor", "as", "async", "class", "const", "debugger", "declare", "delete", "enum", "extends",
        "function", "implements", "in", "infer", "instanceof", "interface", "is", "keyof", "let", "module",
        "namespace", "new", "of", "override", "private", "protected", "public", "readonly", "satisfies", "static",
        "super", "this", "type", "typeof", "unique", "var",
    ],
    types: &["any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown", "void"],
    ..JS
};

static PHP: Syntax = Syntax {
    flavor: Flavor::Php,
    line: &[b"//", b"#"],
    kw: &[
        "abstract", "and", "array", "as", "callable", "class", "clone", "const", "declare", "die", "echo", "empty",
        "enddeclare", "eval", "exit", "extends", "final", "fn", "function", "global", "implements", "include",
        "include_once", "instanceof", "insteadof", "interface", "isset", "list", "namespace", "new", "or", "print",
        "private", "protected", "public", "readonly", "require", "require_once", "static", "trait", "unset", "use",
        "var", "xor",
    ],
    ctl: &[
        "break", "case", "catch", "continue", "default", "do", "else", "elseif", "endfor", "endforeach", "endif",
        "endswitch", "endwhile", "finally", "for", "foreach", "goto", "if", "match", "return", "switch", "throw", "try",
        "while", "yield",
    ],
    types: &["bool", "float", "int", "iterable", "mixed", "never", "object", "string", "void"],
    lits: &["false", "null", "true"],
    sigils: b"$",
    nocase: true,
    cap_types: true,
    ..BASE
};

static RUBY: Syntax = Syntax {
    flavor: Flavor::Ruby,
    line: &[b"#"],
    block: None,
    kw: &["alias", "and", "class", "def", "defined?", "module", "not", "or", "self", "super", "undef"],
    ctl: &[
        "begin", "break", "case", "do", "else", "elsif", "end", "ensure", "for", "if", "in", "next", "redo", "rescue",
        "retry", "return", "then", "unless", "until", "when", "while", "yield",
    ],
    lits: &["false", "nil", "true"],
    quotes: b"\"'`",
    sigils: b"@$",
    cap_types: true,
    ..BASE
};

static LUA: Syntax = Syntax {
    flavor: Flavor::Lua,
    line: &[b"--"],
    block: None,
    kw: &["and", "function", "local", "not", "or"],
    ctl: &[
        "break", "do", "else", "elseif", "end", "for", "goto", "if", "in", "repeat", "return", "then", "until", "while",
    ],
    lits: &["false", "nil", "true"],
    ..BASE
};

static SQL: Syntax = Syntax {
    flavor: Flavor::Sql,
    line: &[b"--"],
    quotes: b"'",
    multi: b"'",
    esc: 0,
    nocase: true,
    kw: &[
        "add", "all", "alter", "and", "any", "as", "asc", "between", "by", "cascade", "check", "collate", "column",
        "constraint", "create", "cross", "database", "declare", "default", "delete", "desc", "distinct", "drop", "exec",
        "execute", "exists", "fetch", "first", "foreign", "from", "full", "function", "grant", "group", "having",
        "identity", "in", "index", "inner", "insert", "intersect", "into", "is", "join", "key", "left", "like", "limit",
        "merge", "natural", "next", "not", "nulls", "offset", "on", "only", "or", "order", "outer", "over", "partition",
        "primary", "procedure", "references", "replace", "returns", "revoke", "right", "row", "rows", "schema",
        "select", "set", "show", "table", "temp", "temporary", "top", "trigger", "truncate", "union", "unique",
        "update", "use", "using", "values", "view", "where", "with",
    ],
    ctl: &["begin", "case", "commit", "else", "end", "if", "loop", "return", "rollback", "then", "transaction", "when", "while"],
    types: &[
        "bigint", "binary", "bit", "blob", "boolean", "char", "clob", "date", "datetime", "datetime2", "datetimeoffset",
        "decimal", "double", "float", "int", "integer", "json", "money", "nchar", "ntext", "numeric", "nvarchar",
        "real", "serial", "smallint", "text", "time", "timestamp", "tinyint", "uniqueidentifier", "uuid", "varbinary",
        "varchar", "xml",
    ],
    lits: &["false", "null", "true"],
    sigils: b"@",
    ..BASE
};

static POWERSHELL: Syntax = Syntax {
    flavor: Flavor::PowerShell,
    line: &[b"#"],
    block: Some((b"<#", b"#>")),
    raw: b"'",
    esc: b'`',
    nocase: true,
    kw: &[
        "begin", "class", "data", "dynamicparam", "end", "enum", "filter", "function", "hidden", "param", "process",
        "static", "using", "workflow",
    ],
    ctl: &[
        "break", "catch", "continue", "do", "else", "elseif", "exit", "finally", "for", "foreach", "if", "in", "return",
        "switch", "throw", "trap", "try", "until", "while",
    ],
    sigils: b"$",
    dash_ident: true,
    ..BASE
};

static BATCH: Syntax = Syntax {
    flavor: Flavor::Batch,
    line: &[],
    block: None,
    quotes: b"\"",
    esc: 0,
    nocase: true,
    kw: &[
        "assoc", "cd", "chdir", "choice", "cls", "color", "copy", "del", "dir", "echo", "endlocal", "erase", "findstr",
        "md", "mkdir", "move", "path", "pause", "popd", "prompt", "pushd", "rd", "ren", "rename", "rmdir", "robocopy",
        "set", "setlocal", "shift", "start", "taskkill", "tasklist", "timeout", "title", "type", "ver", "where",
        "xcopy",
    ],
    ctl: &[
        "call", "defined", "do", "else", "equ", "errorlevel", "exist", "exit", "for", "geq", "goto", "gtr", "if", "in",
        "leq", "lss", "neq", "not",
    ],
    calls: false,
    ..BASE
};

static SHELL: Syntax = Syntax {
    flavor: Flavor::Shell,
    line: &[b"#"],
    block: None,
    quotes: b"\"'`",
    raw: b"'",
    kw: &[
        "alias", "declare", "eval", "exec", "export", "function", "let", "local", "readonly", "set", "shift", "source",
        "trap", "typeset", "unset",
    ],
    ctl: &[
        "break", "case", "continue", "do", "done", "elif", "else", "esac", "exit", "fi", "for", "if", "in", "return",
        "select", "then", "until", "while",
    ],
    lits: &["false", "true"],
    sigils: b"$",
    calls: false,
    ..BASE
};

static DOCKERFILE: Syntax = Syntax {
    kw: &[
        "ADD", "ARG", "AS", "CMD", "COPY", "ENTRYPOINT", "ENV", "EXPOSE", "FROM", "HEALTHCHECK", "LABEL", "MAINTAINER",
        "ONBUILD", "RUN", "SHELL", "STOPSIGNAL", "USER", "VOLUME", "WORKDIR",
    ],
    ctl: &[],
    lits: &[],
    ..SHELL
};

pub(super) fn syntax(lang: Lang) -> &'static Syntax {
    match lang {
        Lang::C | Lang::Cpp => &CPP,
        Lang::CSharp => &CSHARP,
        Lang::Java => &JAVA,
        Lang::Kotlin => &KOTLIN,
        Lang::Swift => &SWIFT,
        Lang::Go => &GO,
        Lang::Rust => &RUST,
        Lang::Python => &PYTHON,
        Lang::JavaScript => &JS,
        Lang::TypeScript => &TS,
        Lang::Php => &PHP,
        Lang::Ruby => &RUBY,
        Lang::Lua => &LUA,
        Lang::Sql => &SQL,
        Lang::PowerShell => &POWERSHELL,
        Lang::Batch => &BATCH,
        Lang::Shell => &SHELL,
        Lang::Dockerfile => &DOCKERFILE,
        _ => &PLAIN,
    }
}

/// After these keywords the next name is a function's / a type's.
const FUNC_KW: [&[u8]; 6] = [b"def", b"fn", b"fun", b"func", b"function", b"sub"];
const TYPE_KW: [&[u8]; 11] =
    [b"class", b"enum", b"interface", b"module", b"object", b"protocol", b"record", b"struct", b"trait", b"type", b"union"];

/// PowerShell operators (`-eq`, `-like`...); other `-Words` are parameter names.
const PS_OPS: [&[u8]; 36] = [
    b"and", b"as", b"band", b"bnot", b"bor", b"bxor", b"ceq", b"cge", b"cgt", b"cle", b"clike", b"clt", b"cmatch", b"cne",
    b"contains", b"eq", b"f", b"ge", b"gt", b"in", b"is", b"isnot", b"join", b"le", b"like", b"lt", b"match", b"ne",
    b"not", b"notcontains", b"notin", b"notlike", b"notmatch", b"or", b"replace", b"split",
];

fn ident_start(sx: &Syntax, c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (sx.dollar_ident && c == b'$')
}

fn ident_char(sx: &Syntax, c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || (sx.dollar_ident && c == b'$')
}

fn ident_end(sx: &Syntax, t: &[u8], mut i: usize) -> usize {
    while i < t.len() {
        let c = t[i];
        if ident_char(sx, c) || (c == b'-' && sx.dash_ident && at(t, i + 1).is_ascii_alphabetic()) {
            i += 1;
        } else {
            break;
        }
    }
    // Ruby: empty?, save!
    if sx.flavor == Flavor::Ruby && matches!(at(t, i), b'?' | b'!') && at(t, i + 1) != b'=' {
        i += 1;
    }
    i
}

fn word_tok(sx: &Syntax, w: &[u8]) -> Option<Tok> {
    let mut buf = [0u8; 24];
    let w = if sx.nocase {
        if w.len() > buf.len() {
            return None;
        }
        for (d, s) in buf.iter_mut().zip(w) {
            *d = s.to_ascii_lowercase();
        }
        &buf[..w.len()]
    } else {
        w
    };
    let has = |list: &[&str]| list.binary_search_by(|k| k.as_bytes().cmp(w)).is_ok();
    if has(sx.ctl) {
        Some(Tok::Control)
    } else if has(sx.kw) {
        Some(Tok::Keyword)
    } else if has(sx.types) {
        Some(Tok::Type)
    } else if has(sx.lits) {
        Some(Tok::Lit)
    } else if has(sx.vars) {
        Some(Tok::Var)
    } else {
        None
    }
}

fn number_end(t: &[u8], i: usize) -> usize {
    let hex = t[i] == b'0' && matches!(at(t, i + 1), b'x' | b'X');
    let mut j = i + 1;
    while j < t.len() {
        let b = t[j];
        if b.is_ascii_alphanumeric() || b == b'_' || (b == b'.' && at(t, j + 1).is_ascii_digit()) {
            j += 1;
        } else if matches!(b, b'+' | b'-') && !hex && matches!(t[j - 1], b'e' | b'E') {
            j += 1;
        } else {
            break;
        }
    }
    j
}

/// What a lexing step did: consumed up to a position, or reached the end still inside something.
type Step = Result<usize, State>;

fn block_rest(sx: &Syntax, t: &[u8], s: usize, mut i: usize, mut depth: u8, st: State, o: &mut Out) -> Step {
    let Some((open, close)) = sx.block else { return Ok(i) };
    loop {
        let next = if sx.nested { memchr::memchr2(open[0], close[0], &t[i..]) } else { memchr::memchr(close[0], &t[i..]) };
        let Some(p) = next else { break };
        i += p;
        if t[i..].starts_with(close) {
            i += close.len();
            depth = depth.saturating_sub(1);
            if depth == 0 {
                o.put(s, i, Tok::Comment);
                return Ok(i);
            }
        } else if sx.nested && t[i..].starts_with(open) {
            i += open.len();
            depth = depth.saturating_add(1);
        } else {
            i += 1;
        }
    }
    o.put(s, t.len(), Tok::Comment);
    Err(State { kind: BLOCK_COMMENT, a: depth, b: 0, ..st })
}

fn string_rest(sx: &Syntax, t: &[u8], s: usize, i: usize, q: u8, esc_pending: bool, st: State, o: &mut Out) -> Step {
    let esc = if sx.raw.contains(&q) { 0 } else { sx.esc };
    let (e, closed, pending) = scan_str(t, i, q, esc, esc_pending, !sx.multi.contains(&q));
    o.put(s, e, Tok::Str);
    if closed { Ok(e) } else { Err(State { kind: STR, a: q, b: pending as u16, ..st }) }
}

fn triple_rest(t: &[u8], s: usize, mut i: usize, q: u8, esc: u8, mut pending: bool, st: State, o: &mut Out) -> Step {
    while i < t.len() {
        let b = t[i];
        if pending {
            pending = false;
        } else if b == esc && esc != 0 {
            pending = true;
        } else if b == q && at(t, i + 1) == q && at(t, i + 2) == q {
            o.put(s, i + 3, Tok::Str);
            return Ok(i + 3);
        }
        i += 1;
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: TRIPLE, a: q, b: pending as u16, ..st })
}

fn raw_rest(t: &[u8], s: usize, mut i: usize, hashes: u8, st: State, o: &mut Out) -> Step {
    while let Some(p) = memchr::memchr(b'"', &t[i..]) {
        i += p + 1;
        let h = t[i..].iter().take(hashes as usize).take_while(|&&b| b == b'#').count();
        if h == hashes as usize {
            o.put(s, i + h, Tok::Str);
            return Ok(i + h);
        }
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: RAW, a: hashes, b: 0, ..st })
}

fn verbatim_rest(t: &[u8], s: usize, mut i: usize, st: State, o: &mut Out) -> Step {
    while let Some(p) = memchr::memchr(b'"', &t[i..]) {
        i += p + 1;
        if at(t, i) == b'"' {
            i += 1;
            continue;
        }
        o.put(s, i, Tok::Str);
        return Ok(i);
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: VERBATIM, a: 0, b: 0, ..st })
}

/// A PowerShell here-string ends at a line starting with `"@` (or `'@`). `line_start`: whether `i` starts a line.
fn here_rest(t: &[u8], s: usize, mut i: usize, q: u8, mut line_start: bool, st: State, o: &mut Out) -> Step {
    loop {
        if line_start && at(t, i) == q && at(t, i + 1) == b'@' {
            o.put(s, i + 2, Tok::Str);
            return Ok(i + 2);
        }
        match memchr::memchr(b'\n', &t[i.min(t.len())..]) {
            Some(p) => {
                i += p + 1;
                line_start = true;
            }
            None => break,
        }
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: HERE, a: q, b: 0, ..st })
}

/// Lua long brackets: `[[`, `[=[`, `[==[`... at `i`: (level, length).
fn long_open(t: &[u8], i: usize) -> Option<(u8, usize)> {
    if at(t, i) != b'[' {
        return None;
    }
    let eq = t[i + 1..].iter().take_while(|&&b| b == b'=').count();
    (at(t, i + 1 + eq) == b'[' && eq < 255).then_some((eq as u8, eq + 2))
}

fn long_rest(t: &[u8], s: usize, mut i: usize, level: u8, comment: bool, st: State, o: &mut Out) -> Step {
    let tok = if comment { Tok::Comment } else { Tok::Str };
    while let Some(p) = memchr::memchr(b']', &t[i..]) {
        i += p + 1;
        let eq = t[i..].iter().take_while(|&&b| b == b'=').count();
        if eq == level as usize && at(t, i + eq) == b']' {
            o.put(s, i + eq + 1, tok);
            return Ok(i + eq + 1);
        }
    }
    o.put(s, t.len(), tok);
    Err(State { kind: LONG, a: level, b: comment as u16, ..st })
}

/// Continues the token the text starts inside.
fn resume(sx: &Syntax, t: &[u8], st: State, o: &mut Out) -> Step {
    match st.kind {
        LINE_COMMENT => {
            let e = line_end(t, 0);
            o.put(0, e, Tok::Comment);
            if e == t.len() { Err(st) } else { Ok(e) }
        }
        BLOCK_COMMENT => block_rest(sx, t, 0, 0, st.a.max(1), st, o),
        STR => string_rest(sx, t, 0, 0, st.a, st.b == 1, st, o),
        TRIPLE => triple_rest(t, 0, 0, st.a, sx.esc, st.b == 1, st, o),
        RAW => raw_rest(t, 0, 0, st.a, st, o),
        VERBATIM => verbatim_rest(t, 0, 0, st, o),
        HERE => here_rest(t, 0, 0, st.a, st.col0, st, o),
        LONG => long_rest(t, 0, 0, st.a, st.b == 1, st, o),
        _ => Ok(0),
    }
}

fn line_comment(sx: &Syntax, t: &[u8], i: usize) -> bool {
    sx.line.iter().any(|lc| {
        t[i..].starts_with(lc)
            // shells: `#` starts a comment only at the start of a word (not in `a#b` or `${#x}`)
            && !(sx.flavor == Flavor::Shell && i > 0 && !matches!(t[i - 1], b' ' | b'\t' | b';' | b'|' | b'&' | b'('))
    })
}

/// A Batch variable at `i`: `%name%`, `%1`, `%~dp0`, `%%i`, `!name!`. Returns its end.
fn batch_var(t: &[u8], i: usize) -> Option<usize> {
    match t[i] {
        b'%' => {
            let c = at(t, i + 1);
            if c == b'%' && at(t, i + 2).is_ascii_alphabetic() {
                return Some(i + 3);
            }
            if c.is_ascii_digit() || c == b'*' {
                return Some(i + 2);
            }
            if c == b'~' {
                let e = i + 2 + t[i + 2..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
                return (e < t.len() && (t[e].is_ascii_digit() || t[e] == b'*')).then_some(e + 1);
            }
            let e = i + 1 + t[i + 1..].iter().take(64).take_while(|&&b| !matches!(b, b'%' | b' ' | b'\n' | b'"')).count();
            (at(t, e) == b'%' && e > i + 1).then_some(e + 1)
        }
        b'!' => {
            let e = i + 1 + t[i + 1..].iter().take(64).take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_').count();
            (at(t, e) == b'!' && e > i + 1).then_some(e + 1)
        }
        _ => None,
    }
}

/// A variable at `i` starting with one of `sx.sigils`: its end, or None.
fn sigil_var(sx: &Syntax, t: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    // Ruby @@class_var, SQL @@ROWCOUNT
    if at(t, j) == t[i] && t[i] == b'@' {
        j += 1;
    }
    let c = at(t, j);
    if c == b'{' {
        let e = t[j..].iter().take(256).position(|&b| b == b'}' || b == b'\n')?;
        return (t[j + e] == b'}').then_some(j + e + 1);
    }
    if sx.flavor == Flavor::Shell && (c.is_ascii_digit() || matches!(c, b'@' | b'#' | b'?' | b'$' | b'!' | b'*' | b'-')) {
        return Some(j + 1);
    }
    if !(c.is_ascii_alphabetic() || c == b'_' || c >= 0x80) {
        return None;
    }
    let mut e = j;
    while e < t.len() && (t[e].is_ascii_alphanumeric() || t[e] == b'_' || t[e] >= 0x80) {
        e += 1;
    }
    // PowerShell scopes: $env:PATH, $script:count
    if sx.flavor == Flavor::PowerShell && at(t, e) == b':' && at(t, e + 1).is_ascii_alphabetic() {
        e += 1;
        while e < t.len() && (t[e].is_ascii_alphanumeric() || t[e] == b'_') {
            e += 1;
        }
    }
    Some(e)
}

/// Per-language extras at `i` that come before the general rules. Some(step) when one applied.
fn extras(sx: &Syntax, t: &[u8], i: usize, st: State, o: &mut Out) -> Option<Step> {
    let c = t[i];
    let n = t.len();
    match sx.flavor {
        Flavor::Rust => {
            // raw strings r"…", r#"…"#, br"…"
            let r = if c == b'r' { i + 1 } else if c == b'b' && at(t, i + 1) == b'r' { i + 2 } else { 0 };
            if r > 0 && (i == 0 || !ident_char(sx, t[i - 1])) {
                let h = t[r..].iter().take(255).take_while(|&&b| b == b'#').count();
                if at(t, r + h) == b'"' {
                    return Some(raw_rest(t, i, r + h + 1, h as u8, st, o));
                }
            }
            if c == b'\'' {
                // a char literal ('a', '\n', '日') or a lifetime ('a, 'static)
                if at(t, i + 1) == b'\\' {
                    let e = t[i + 2..].iter().take(12).position(|&b| b == b'\'').map_or(i + 2, |p| i + 3 + p);
                    o.put(i, e, Tok::Str);
                    return Some(Ok(e));
                }
                let len = crate::core::text::char_len_at(&t[i + 1..]).max(1);
                if at(t, i + 1 + len) == b'\'' {
                    o.put(i, i + 2 + len, Tok::Str);
                    return Some(Ok(i + 2 + len));
                }
                if ident_start(sx, at(t, i + 1)) {
                    let e = ident_end(sx, t, i + 1);
                    o.put(i, e, Tok::Keyword);
                    return Some(Ok(e));
                }
                return Some(Ok(i + 1));
            }
            // attributes #[...] / #![...]
            if c == b'#' && (at(t, i + 1) == b'[' || (at(t, i + 1) == b'!' && at(t, i + 2) == b'[')) {
                let le = line_end(t, i);
                let mut depth = 0;
                let mut e = le;
                for (k, &b) in t[i..le].iter().enumerate() {
                    if b == b'[' {
                        depth += 1;
                    } else if b == b']' {
                        depth -= 1;
                        if depth == 0 {
                            e = i + k + 1;
                            break;
                        }
                    }
                }
                o.put(i, e, Tok::Control);
                return Some(Ok(e));
            }
        }
        Flavor::CSharp => {
            // @"verbatim", $"interpolated", $@"both", @$"both", $"""raw"""
            let verbatim = (c == b'@' && at(t, i + 1) == b'"')
                .then_some(i + 2)
                .or_else(|| (matches!(&t[i..n.min(i + 3)], b"$@\"" | b"@$\"")).then_some(i + 3));
            if let Some(from) = verbatim {
                return Some(verbatim_rest(t, i, from, st, o));
            }
            if c == b'$' && at(t, i + 1) == b'"' {
                if t[i + 1..].starts_with(b"\"\"\"") {
                    return Some(triple_rest(t, i, i + 4, b'"', 0, false, st, o));
                }
                return Some(string_rest(sx, t, i, i + 2, b'"', false, st, o));
            }
        }
        Flavor::Lua => {
            if t[i..].starts_with(b"--") {
                if let Some((level, len)) = long_open(t, i + 2) {
                    return Some(long_rest(t, i, i + 2 + len, level, true, st, o));
                }
            }
            if let Some((level, len)) = long_open(t, i) {
                return Some(long_rest(t, i, i + len, level, false, st, o));
            }
        }
        Flavor::PowerShell => {
            // here-strings: @" or @' at the end of a line
            if c == b'@' && matches!(at(t, i + 1), b'"' | b'\'') && t[i + 2..line_end(t, i)].trim_ascii().is_empty() {
                return Some(here_rest(t, i, i + 2, t[i + 1], false, st, o));
            }
            // -eq, -like (operators) and -Path, -Force (parameters)
            if c == b'-' && at(t, i + 1).is_ascii_alphabetic() && (i == 0 || matches!(t[i - 1], b' ' | b'\t' | b'(' | b'{' | b',' | b'|' | b'=')) {
                let e = i + 1 + t[i + 1..].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
                let w = t[i + 1..e].to_ascii_lowercase();
                o.put(i, e, if PS_OPS.contains(&w.as_slice()) { Tok::Keyword } else { Tok::Var });
                return Some(Ok(e));
            }
            // [type] literals: [string], [System.IO.Path]
            if c == b'[' && at(t, i + 1).is_ascii_alphabetic() && (i == 0 || !(ident_char(sx, t[i - 1]) || matches!(t[i - 1], b')' | b']' | b'}'))) {
                let mut depth = 0;
                for (k, &b) in t[i..n.min(i + 120)].iter().enumerate() {
                    match b {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                o.put(i + 1, i + k, Tok::Type);
                                return Some(Ok(i + k + 1));
                            }
                        }
                        b'.' | b',' | b' ' | b'`' | b'_' => {}
                        b if b.is_ascii_alphanumeric() => {}
                        _ => break,
                    }
                }
            }
            if c == b'$' {
                let e = sigil_var(sx, t, i)?;
                let w = t[i + 1..e].to_ascii_lowercase();
                o.put(i, e, if matches!(w.as_slice(), b"true" | b"false" | b"null") { Tok::Lit } else { Tok::Var });
                return Some(Ok(e));
            }
        }
        Flavor::Ruby => {
            // :symbols
            if c == b':' && ident_start(sx, at(t, i + 1)) && (i == 0 || (t[i - 1] != b':' && !ident_char(sx, t[i - 1]))) {
                let e = ident_end(sx, t, i + 1);
                o.put(i, e, Tok::Lit);
                return Some(Ok(e));
            }
        }
        Flavor::Php => {
            for tag in [&b"<?php"[..], b"<?=", b"?>"] {
                if t[i..].starts_with(tag) {
                    o.put(i, i + tag.len(), Tok::Control);
                    return Some(Ok(i + tag.len()));
                }
            }
        }
        Flavor::Sql => {
            // "quoted", `quoted` and [bracketed] names
            if c == b'"' || c == b'`' {
                let (e, _, _) = scan_str(t, i + 1, c, 0, false, true);
                o.put(i, e, Tok::Var);
                return Some(Ok(e));
            }
            if c == b'[' {
                let le = line_end(t, i);
                if let Some(p) = t[i + 1..le].iter().position(|&b| b == b']' || b == b'[') {
                    if t[i + 1 + p] == b']' && p > 0 {
                        o.put(i, i + p + 2, Tok::Var);
                        return Some(Ok(i + p + 2));
                    }
                }
            }
        }
        Flavor::Plain | Flavor::Batch | Flavor::Shell => {}
    }
    None
}

pub(super) fn code(sx: &Syntax, t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut i = 0;
    if st.kind != 0 {
        match resume(sx, t, st, o) {
            Ok(e) => i = e,
            Err(s) => return s,
        }
    }
    let mid = State { kind: 0, a: 0, b: 0, ..st };
    let mut bol = st.bol && i == 0;
    // Batch: at the start of a statement (where `rem` starts a comment); inside an echo (plain text).
    let mut stmt = bol;
    let mut echo = false;
    // After `def` / `class`: the next name is a function's (1) / a type's (2).
    let mut expect = 0u8;
    macro_rules! step {
        ($e:expr) => {
            match $e {
                Ok(e) => {
                    i = e;
                    continue;
                }
                Err(s) => return s,
            }
        };
    }
    while i < n {
        let c = t[i];
        match c {
            b'\n' => {
                (bol, stmt, echo, expect) = (true, true, false, 0);
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let line_start = bol;
        let stmt_start = stmt;
        bol = false;
        stmt = false;
        if sx.flavor == Flavor::Batch {
            if let Some(e) = batch_var(t, i) {
                o.put(i, e, Tok::Var);
                i = e;
                continue;
            }
            if matches!(c, b'&' | b'|' | b'(') {
                (stmt, echo) = (true, false);
                i += 1;
                continue;
            }
            if echo {
                i += 1;
                continue;
            }
            if line_start && c == b'@' {
                o.put(i, i + 1, Tok::Dim);
                (bol, stmt) = (true, true);
                i += 1;
                continue;
            }
            if line_start && c == b':' {
                // ::comment or :label
                let e = line_end(t, i);
                o.put(i, e, if at(t, i + 1) == b':' { Tok::Comment } else { Tok::Section });
                i = e;
                continue;
            }
        }
        if let Some(r) = extras(sx, t, i, mid, o) {
            step!(r);
        }
        if line_comment(sx, t, i) {
            let e = line_end(t, i);
            o.put(i, e, Tok::Comment);
            if e == n {
                return State { kind: LINE_COMMENT, ..mid };
            }
            i = e;
            continue;
        }
        if let Some((open, _)) = sx.block {
            if t[i..].starts_with(open) {
                step!(block_rest(sx, t, i, i + open.len(), 1, mid, o));
            }
        }
        if sx.preproc && c == b'#' && line_start {
            // #include <x.h>, #define, #region
            let w = i + 1 + t[i + 1..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
            let e = w + t[w..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            o.put(i, e, Tok::Control);
            if matches!(&t[w..e], b"include" | b"import") {
                let s = e + t[e..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
                if at(t, s) == b'<' {
                    let le = line_end(t, s);
                    let close = t[s..le].iter().position(|&b| b == b'>').map_or(le, |p| s + p + 1);
                    o.put(s, close, Tok::Str);
                    i = close;
                    continue;
                }
            }
            i = e;
            continue;
        }
        if sx.decorators && c == b'@' && ident_start(sx, at(t, i + 1)) {
            let mut e = ident_end(sx, t, i + 1);
            while at(t, e) == b'.' && ident_start(sx, at(t, e + 1)) {
                e = ident_end(sx, t, e + 1);
            }
            o.put(i, e, Tok::Func);
            i = e;
            continue;
        }
        if sx.sigils.contains(&c) {
            if let Some(e) = sigil_var(sx, t, i) {
                o.put(i, e, Tok::Var);
                i = e;
                continue;
            }
        }
        if sx.triple.contains(&c) && at(t, i + 1) == c && at(t, i + 2) == c {
            step!(triple_rest(t, i, i + 3, c, sx.esc, false, mid, o));
        }
        if sx.quotes.contains(&c) {
            step!(string_rest(sx, t, i, i + 1, c, false, mid, o));
        }
        if c.is_ascii_digit() || (c == b'.' && at(t, i + 1).is_ascii_digit() && (i == 0 || !ident_char(sx, t[i - 1]))) {
            let e = number_end(t, i);
            o.put(i, e, Tok::Num);
            expect = 0;
            i = e;
            continue;
        }
        if ident_start(sx, c) {
            let mut e = ident_end(sx, t, i);
            let w = &t[i..e];
            if !o.on() && sx.flavor != Flavor::Batch {
                i = e;
                continue;
            }
            // after a '.', a keyword is a member name (`re.match`, `map.get`, `promise.catch`)
            let member = i > 0 && t[i - 1] == b'.' && !(i > 1 && t[i - 2] == b'.');
            let mut tok = if member { None } else { word_tok(sx, w) };
            if sx.flavor == Flavor::Batch {
                let ends = matches!(at(t, e), 0 | b' ' | b'\t' | b'\r' | b'\n');
                if stmt_start && ends && w.eq_ignore_ascii_case(b"rem") {
                    let le = line_end(t, i);
                    o.put(i, le, Tok::Comment);
                    i = le;
                    continue;
                }
                if w.eq_ignore_ascii_case(b"echo") {
                    echo = true;
                }
            }
            if tok.is_none() {
                let next = t[e..].iter().copied().find(|&b| b != b' ' && b != b'\t');
                tok = if expect == 1 {
                    Some(Tok::Func)
                } else if expect == 2 {
                    Some(Tok::Type)
                } else if sx.flavor == Flavor::Rust && at(t, e) == b'!' && at(t, e + 1) != b'=' {
                    e += 1;
                    Some(Tok::Func)
                } else if sx.dash_ident && w.contains(&b'-') {
                    Some(Tok::Func)
                } else if sx.cap_types && w[0].is_ascii_uppercase() && w.iter().any(|b| b.is_ascii_lowercase()) {
                    Some(Tok::Type)
                } else if sx.calls && next == Some(b'(') {
                    Some(Tok::Func)
                } else {
                    None
                };
            }
            expect = 0;
            if tok == Some(Tok::Keyword) {
                let lower = w.to_ascii_lowercase();
                if FUNC_KW.contains(&lower.as_slice()) {
                    expect = 1;
                } else if TYPE_KW.contains(&lower.as_slice()) {
                    expect = 2;
                }
            }
            if let Some(tk) = tok {
                o.put(i, e, tk);
            }
            i = e;
            continue;
        }
        expect = 0;
        i += 1;
    }
    mid
}

// ---- CSS (also SCSS and LESS) ----

const CSS_COMMENT: u8 = 1;
const CSS_STR: u8 = 2;

fn css_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80
}

fn css_ident_end(t: &[u8], mut i: usize) -> usize {
    while i < t.len() && css_ident_char(t[i]) {
        i += 1;
    }
    i
}

/// `b` holds the brace depth: 0 is where selectors are, deeper is inside rules (properties and values).
pub(super) fn css(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut depth = st.b;
    let mut i = 0;
    match st.kind {
        CSS_COMMENT => match find(t, 0, b"*/") {
            Some(p) => {
                o.put(0, p + 2, Tok::Comment);
                i = p + 2;
            }
            None => {
                o.put(0, n, Tok::Comment);
                return st;
            }
        },
        CSS_STR => {
            let (e, closed, _) = scan_str(t, 0, st.a, b'\\', false, true);
            o.put(0, e, Tok::Str);
            if !closed {
                return st;
            }
            i = e;
        }
        _ => {}
    }
    while i < n {
        let c = t[i];
        let next = at(t, i + 1);
        match c {
            b'/' if next == b'*' => match find(t, i + 2, b"*/") {
                Some(p) => {
                    o.put(i, p + 2, Tok::Comment);
                    i = p + 2;
                }
                None => {
                    o.put(i, n, Tok::Comment);
                    return State { kind: CSS_COMMENT, a: 0, b: depth, ..st };
                }
            },
            // SCSS / LESS line comments (not the `//` of a URL)
            b'/' if next == b'/' && (i == 0 || matches!(t[i - 1], b' ' | b'\t' | b';' | b'{' | b'}')) => {
                let e = line_end(t, i);
                o.put(i, e, Tok::Comment);
                i = e;
            }
            b'"' | b'\'' => {
                let (e, closed, _) = scan_str(t, i + 1, c, b'\\', false, true);
                o.put(i, e, Tok::Str);
                if !closed {
                    return State { kind: CSS_STR, a: c, b: depth, ..st };
                }
                i = e;
            }
            b'{' => {
                depth = depth.saturating_add(1);
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'@' if css_ident_char(next) => {
                let e = css_ident_end(t, i + 1);
                o.put(i, e, Tok::Control);
                i = e;
            }
            b'$' if css_ident_char(next) => {
                let e = css_ident_end(t, i + 1);
                o.put(i, e, Tok::Var);
                i = e;
            }
            b'-' if next == b'-' && css_ident_char(at(t, i + 2)) => {
                let e = css_ident_end(t, i + 2);
                o.put(i, e, Tok::Var);
                i = e;
            }
            b'!' if t.len() >= i + 10 && t[i + 1..i + 10].eq_ignore_ascii_case(b"important") => {
                o.put(i, i + 10, Tok::Control);
                i += 10;
            }
            b'#' if css_ident_char(next) => {
                // a color inside rules, an id in selectors
                let e = css_ident_end(t, i + 1);
                o.put(i, e, if depth > 0 { Tok::Num } else { Tok::Func });
                i = e;
            }
            b'.' if !next.is_ascii_digit() && css_ident_char(next) && next != b'-' => {
                let e = css_ident_end(t, i + 1);
                o.put(i, e, Tok::Func);
                i = e;
            }
            b':' if depth == 0 && (css_ident_char(next) || next == b':') => {
                // :hover, ::before
                let s = if next == b':' { i + 2 } else { i + 1 };
                let e = css_ident_end(t, s);
                o.put(i, e, Tok::Control);
                i = e;
            }
            b'0'..=b'9' => {
                let e = i + t[i..].iter().take_while(|&&b| b.is_ascii_digit() || b == b'.').count();
                let e = e + t[e..].iter().take_while(|&&b| b.is_ascii_alphabetic() || b == b'%').count();
                o.put(i, e, Tok::Num);
                i = e;
            }
            b'.' | b'-' if next.is_ascii_digit() => {
                let e = i + 1 + t[i + 1..].iter().take_while(|&&b| b.is_ascii_digit() || b == b'.').count();
                let e = e + t[e..].iter().take_while(|&&b| b.is_ascii_alphabetic() || b == b'%').count();
                o.put(i, e, Tok::Num);
                i = e;
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (c == b'-' && css_ident_char(next)) => {
                let e = css_ident_end(t, i);
                if at(t, e) == b'(' {
                    if t[i..e].eq_ignore_ascii_case(b"url") {
                        // url(...) as a whole, // and all
                        let le = line_end(t, e);
                        let (inner, close) = match t[e..le].iter().position(|&b| b == b')') {
                            Some(p) => (e + p, e + p + 1),
                            None => (le, le),
                        };
                        o.put(i, e, Tok::Func);
                        o.put(e + 1, inner, Tok::Str);
                        i = close;
                        continue;
                    }
                    o.put(i, e, Tok::Func);
                } else if depth == 0 {
                    o.put(i, e, Tok::Tag);
                } else {
                    let after = t[e..].iter().copied().find(|&b| b != b' ' && b != b'\t');
                    o.put(i, e, if after == Some(b':') && at(t, e + 1) != b':' { Tok::Attr } else { Tok::Str });
                }
                i = e;
            }
            _ => i += 1,
        }
    }
    State { kind: 0, a: 0, b: depth, ..st }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{end_state, has, toks};
    use super::super::{Lang, State, Tok};

    #[test]
    fn keyword_lists_are_sorted() {
        for lang in Lang::ALL {
            let sx = super::syntax(lang);
            for list in [sx.kw, sx.ctl, sx.types, sx.lits, sx.vars] {
                for w in list.windows(2) {
                    assert!(w[0] < w[1], "{lang:?}: {:?} before {:?}", w[0], w[1]);
                }
                if sx.nocase {
                    assert!(list.iter().all(|k| k.bytes().all(|b| !b.is_ascii_uppercase())), "{lang:?} lowercase");
                }
            }
        }
    }

    #[test]
    fn c_family() {
        has(Lang::Cpp, "#include <stdio.h>", &[("#include", Tok::Control), ("<stdio.h>", Tok::Str)]);
        has(Lang::Cpp, "int main() { return 0; } // done", &[
            ("int", Tok::Type),
            ("main", Tok::Func),
            ("return", Tok::Control),
            ("0", Tok::Num),
            ("// done", Tok::Comment),
        ]);
        has(Lang::CSharp, r#"var s = @"C:\path""quoted"; class Foo : IBar {}"#, &[
            ("var", Tok::Keyword),
            (r#"@"C:\path""quoted""#, Tok::Str),
            ("Foo", Tok::Type),
            ("IBar", Tok::Type),
        ]);
        has(Lang::Java, "@Override public void run() {}", &[("@Override", Tok::Func), ("public", Tok::Keyword), ("void", Tok::Type)]);
        has(Lang::JavaScript, "const x = `a ${b}`; obj.delete(1); function go() {}", &[
            ("const", Tok::Keyword),
            ("`a ${b}`", Tok::Str),
            ("delete", Tok::Func),
            ("go", Tok::Func),
        ]);
        has(Lang::Go, "func main() { s := `raw\\` }", &[("func", Tok::Keyword), ("main", Tok::Func), ("`raw\\`", Tok::Str)]);
    }

    #[test]
    fn multi_line_constructs() {
        // a block comment over lines, followed by code
        let st = end_state(Lang::C, "int a; /* start\nstill comment\n");
        assert_eq!(toks(Lang::C, "end */ int b;", st)[0], ("end */".into(), Tok::Comment));
        // Python triple-quoted strings
        let st = end_state(Lang::Python, "def f():\n    \"\"\"Docs\n");
        let t = toks(Lang::Python, "    for the function\"\"\" return", st);
        assert_eq!(t[0], ("    for the function\"\"\"".into(), Tok::Str));
        // Rust nested comments and raw strings
        let st = end_state(Lang::Rust, "/* a /* b */ still\n");
        assert_eq!(toks(Lang::Rust, "c */ fn x() {}", st)[0], ("c */".into(), Tok::Comment));
        let st = end_state(Lang::Rust, "let s = r#\"line \"one\"\n");
        assert_eq!(toks(Lang::Rust, "two\"# ;", st)[0], ("two\"#".into(), Tok::Str));
        // Lua long comments, PowerShell here-strings and block comments
        let st = end_state(Lang::Lua, "--[==[ note\n");
        assert_eq!(toks(Lang::Lua, "]] still ]==] local x", st)[0], ("]] still ]==]".into(), Tok::Comment));
        let st = end_state(Lang::PowerShell, "$s = @\"\nText \"quoted\"\n");
        assert_eq!(toks(Lang::PowerShell, "\"@", st)[0], ("\"@".into(), Tok::Str));
        let st = end_state(Lang::PowerShell, "<# help\n");
        assert_eq!(toks(Lang::PowerShell, "#> Get-Item", st)[0], ("#>".into(), Tok::Comment));
    }

    #[test]
    fn scripting_languages() {
        has(Lang::Python, "@dataclass\nclass Point(Base): # note\n    def area(self): return None", &[
            ("@dataclass", Tok::Func),
            ("class", Tok::Keyword),
            ("Point", Tok::Type),
            ("# note", Tok::Comment),
            ("area", Tok::Func),
            ("self", Tok::Var),
            ("None", Tok::Lit),
        ]);
        has(Lang::PowerShell, "Get-ChildItem -Path $env:TEMP -Force | Where-Object { $_.Length -gt 1KB } # files", &[
            ("Get-ChildItem", Tok::Func),
            ("-Path", Tok::Var),
            ("$env:TEMP", Tok::Var),
            ("-gt", Tok::Keyword),
            ("Where-Object", Tok::Func),
            ("# files", Tok::Comment),
        ]);
        has(Lang::PowerShell, "[string]$name = 'x'; if ($true) {}", &[("string", Tok::Type), ("'x'", Tok::Str), ("$true", Tok::Lit), ("if", Tok::Control)]);
        has(Lang::Batch, "@echo off\nREM a note\n:loop\nset X=%~dp0\nif exist \"%X%\" goto loop", &[
            ("@", Tok::Dim),
            ("echo", Tok::Keyword),
            ("REM a note", Tok::Comment),
            (":loop", Tok::Section),
            ("%~dp0", Tok::Var),
            ("exist", Tok::Control),
            ("goto", Tok::Control),
        ]);
        has(Lang::Shell, "#!/bin/bash\nfor f in *.txt; do echo \"$f\" ${HOME} $1; done # end", &[
            ("#!/bin/bash", Tok::Comment),
            ("for", Tok::Control),
            ("${HOME}", Tok::Var),
            ("$1", Tok::Var),
            ("# end", Tok::Comment),
        ]);
        has(Lang::Sql, "SELECT name, COUNT(*) FROM [users] WHERE id = @id -- all", &[
            ("SELECT", Tok::Keyword),
            ("COUNT", Tok::Func),
            ("[users]", Tok::Var),
            ("@id", Tok::Var),
            ("-- all", Tok::Comment),
        ]);
        has(Lang::Rust, "#[derive(Debug)] fn main<'a>(x: &'a str) { println!(\"{}\", 'c'); }", &[
            ("#[derive(Debug)]", Tok::Control),
            ("main", Tok::Func),
            ("'a", Tok::Keyword),
            ("str", Tok::Type),
            ("println!", Tok::Func),
            ("'c'", Tok::Str),
        ]);
        has(Lang::Ruby, "def save!(x) :ok end", &[("save!", Tok::Func), (":ok", Tok::Lit), ("end", Tok::Control)]);
        has(Lang::Dockerfile, "FROM alpine AS build\nRUN echo $HOME # x", &[("FROM", Tok::Keyword), ("$HOME", Tok::Var), ("# x", Tok::Comment)]);
    }

    #[test]
    fn css_rules() {
        has(Lang::Css, "a.btn:hover, #main { color: #fff !important; margin: 0 2px; background: url(//x.png) } /* c */", &[
            ("a", Tok::Tag),
            (".btn", Tok::Func),
            (":hover", Tok::Control),
            ("#main", Tok::Func),
            ("color", Tok::Attr),
            ("#fff", Tok::Num),
            ("!important", Tok::Control),
            ("2px", Tok::Num),
            ("//x.png", Tok::Str),
            ("/* c */", Tok::Comment),
        ]);
        let st = end_state(Lang::Css, "@media print {\n  body {\n");
        assert!(toks(Lang::Css, "    color: red;", st).contains(&("color".into(), Tok::Attr)));
        assert!(toks(Lang::Css, "    color: red;", State::START).contains(&("color".into(), Tok::Tag)));
    }
}
