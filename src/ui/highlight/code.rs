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
/// A JavaScript `/regex/`; `a`: 1 inside `[…]`, +2 if an escape is pending.
const REGEX: u8 = 9;
/// A shell or Ruby heredoc's lines (`<<EOF` … `EOF`); `a`: 1 if the end line may be indented, +2 while the line
/// that starts it isn't over yet, and bits 2..7 with `b` hold a hash of the end word.
const HEREDOC: u8 = 10;
/// Perl documentation, `=head1` … `=cut`.
const POD: u8 = 11;
/// Perl: everything after `__END__` / `__DATA__`.
const DATA: u8 = 12;
/// Perl's `q(…)`, `qw[…]`, `m{…}`, `s/…/…/`, `tr/…/…/`…; `a`: the closing delimiter (0 between the two parts of
/// `s{…}{…}`), `b`: the nesting depth of bracket delimiters (bits 0-5), another part to come (bit 6), an escape
/// pending (bit 7), and the lines so far (bits 8-15).
const QUOTE_LIKE: u8 = 13;
/// A C++ raw string `R"delim(…)delim"`; `a` and `b` hold a hash of `delim` (as for heredocs).
const CPP_RAW: u8 = 14;
/// An interpolated string on one line (C# `$"…{x}…"`, HCL `"…${x}…"`); `a`: bit 0 an escape pending, bit 1 inside
/// a `{…}` hole, bit 2 inside a string in the hole, bit 3 an escape pending there; `b`: the hole's brace depth.
const INTERP: u8 = 15;

/// In `STR`'s `a`: a single quote with backslash escapes (shell `$'…'`, SQL `E'…'`).
const ESC_QUOTE: u8 = 0x80 | b'\'';
/// In SQL's `State::mode`: the text is MySQL's (`\` escapes in strings, `#` comments), as seen from a backtick,
/// a `/*!` comment or a comment that names it.
const MYSQL: u8 = 1;
/// In AutoHotkey's `State::mode`: the script is for version 2 (`#Requires AutoHotkey v2`), where `'` quotes too.
const AHK_V2: u8 = 1;
/// A Perl quote-like construct gives up after this many lines (a misread `s` or `y` mustn't color the whole file).
const MAX_QUOTE_LINES: u8 = 200;
/// A string that may span lines in a language where a stray quote is easily mistaken for one (shell, SQL...) ends
/// at a blank line or after this many lines, so a wrong guess can't recolor the rest of the file.
const MAX_STR_LINES: u16 = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Plain,
    Cpp,
    Rust,
    CSharp,
    Lua,
    Sql,
    PowerShell,
    Batch,
    Shell,
    Ruby,
    Php,
    Vb,
    Ahk,
    Perl,
    R,
    Hcl,
    CMake,
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
    /// Strings that may span lines give up at a blank line or after `MAX_STR_LINES` (see there).
    resync: bool,
    /// `/regex/` literals (JavaScript).
    regex: bool,
    /// `'` between digits separates them (`1'000'000` in C and C++).
    digit_sep: bool,
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
    resync: false,
    regex: false,
    digit_sep: false,
};

static PLAIN: Syntax = BASE;

static C: Syntax = Syntax {
    kw: &[
        "_Alignas", "_Alignof", "_Atomic", "_Generic", "_Noreturn", "_Static_assert", "_Thread_local", "alignas",
        "alignof", "auto", "const", "constexpr", "enum", "extern", "inline", "register", "restrict", "signed", "sizeof",
        "static", "static_assert", "struct", "thread_local", "typedef", "typeof", "typeof_unqual", "union", "unsigned",
        "volatile",
    ],
    ctl: &["break", "case", "continue", "default", "do", "else", "for", "goto", "if", "return", "switch", "while"],
    types: &[
        "_Bool", "_Complex", "_Imaginary", "bool", "char", "char16_t", "char32_t", "char8_t", "double", "float", "int",
        "int16_t", "int32_t", "int64_t", "int8_t", "long", "ptrdiff_t", "short", "size_t", "ssize_t", "uint16_t",
        "uint32_t", "uint64_t", "uint8_t", "uintptr_t", "void", "wchar_t",
    ],
    lits: &["NULL", "false", "nullptr", "true"],
    preproc: true,
    digit_sep: true,
    ..BASE
};

static CPP: Syntax = Syntax {
    flavor: Flavor::Cpp,
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
    digit_sep: true,
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
    regex: true,
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
    multi: b"\"'",
    sigils: b"$",
    nocase: true,
    cap_types: true,
    resync: true,
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
    multi: b"\"'`",
    sigils: b"@$",
    cap_types: true,
    resync: true,
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
    resync: true,
    ..BASE
};

static POWERSHELL: Syntax = Syntax {
    flavor: Flavor::PowerShell,
    line: &[b"#"],
    block: Some((b"<#", b"#>")),
    multi: b"\"'",
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
    resync: true,
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
    multi: b"\"'`",
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
    resync: true,
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

/// VBScript, VBA and Visual Basic: `'` and `Rem` comments, `""` inside strings, `&HFF`, `#1/2/2026#`.
static VB: Syntax = Syntax {
    flavor: Flavor::Vb,
    line: &[b"'"],
    block: None,
    quotes: b"\"",
    esc: 0,
    nocase: true,
    kw: &[
        "addhandler", "addressof", "alias", "and", "andalso", "as", "byref", "byval", "class", "const", "declare",
        "delegate", "dim", "enum", "erase", "event", "explicit", "friend", "function", "get", "global", "handles",
        "implements", "imports", "inherits", "interface", "is", "isnot", "let", "lib", "like", "mod", "module",
        "mustinherit", "mustoverride", "namespace", "new", "not", "of", "option", "optional", "or", "orelse",
        "overloads", "overridable", "overrides", "paramarray", "preserve", "private", "property", "protected",
        "ptrsafe", "public", "raiseevent", "readonly", "redim", "removehandler", "set", "shadows", "shared", "static",
        "structure", "sub", "type", "typeof", "withevents", "writeonly", "xor",
    ],
    ctl: &[
        "call", "case", "catch", "continue", "do", "each", "else", "elseif", "end", "exit", "finally", "for", "goto",
        "if", "in", "loop", "next", "on", "resume", "return", "select", "step", "stop", "then", "throw", "to", "try",
        "until", "wend", "when", "while", "with",
    ],
    types: &[
        "boolean", "byte", "char", "currency", "date", "decimal", "double", "integer", "long", "longlong", "longptr",
        "object", "sbyte", "short", "single", "string", "uinteger", "ulong", "ushort", "variant",
    ],
    lits: &["empty", "false", "nothing", "null", "true"],
    vars: &["me", "mybase", "myclass"],
    // #If, #Const, #Region
    preproc: true,
    ..BASE
};

/// AutoHotkey v1 and v2: `;` comments after a space, `/* */` from a line's start, backtick escapes, hotkeys and
/// hotstrings (`^!s::`, `::btw::by the way`), `%var%`.
static AHK: Syntax = Syntax {
    flavor: Flavor::Ahk,
    line: &[b";"],
    quotes: b"\"",
    esc: b'`',
    nocase: true,
    kw: &["and", "class", "extends", "global", "in", "is", "local", "new", "not", "or", "static", "super", "this"],
    ctl: &[
        "break", "case", "catch", "continue", "default", "else", "finally", "for", "gosub", "goto", "if", "loop",
        "return", "switch", "throw", "try", "until", "while",
    ],
    lits: &["false", "true", "unset"],
    // #Requires, #Include, #SingleInstance
    preproc: true,
    ..BASE
};

static PERL: Syntax = Syntax {
    flavor: Flavor::Perl,
    line: &[b"#"],
    block: None,
    quotes: b"\"'`",
    multi: b"\"'`",
    kw: &[
        "and", "bless", "chomp", "chop", "close", "cmp", "defined", "delete", "die", "each", "eq", "eval", "exists",
        "ge", "gt", "keys", "le", "local", "lt", "my", "ne", "no", "not", "open", "or", "our", "package", "pop", "print",
        "printf", "push", "ref", "require", "say", "scalar", "shift", "sort", "splice", "split", "sprintf", "state",
        "sub", "unshift", "use", "values", "wantarray", "warn", "xor",
    ],
    ctl: &[
        "default", "do", "else", "elsif", "for", "foreach", "given", "goto", "if", "last", "next", "redo", "return",
        "unless", "until", "when", "while",
    ],
    lits: &["__DATA__", "__END__", "__FILE__", "__LINE__", "__PACKAGE__", "__SUB__", "undef"],
    sigils: b"$@%",
    resync: true,
    regex: true,
    ..BASE
};

static R: Syntax = Syntax {
    flavor: Flavor::R,
    line: &[b"#"],
    block: None,
    // (`backtick names` are colored like strings)
    quotes: b"\"'`",
    multi: b"\"'",
    kw: &["function", "in"],
    ctl: &["break", "else", "for", "if", "next", "repeat", "return", "while"],
    lits: &["F", "FALSE", "Inf", "NA", "NA_character_", "NA_complex_", "NA_integer_", "NA_real_", "NULL", "NaN", "T", "TRUE"],
    resync: true,
    ..BASE
};

/// Terraform and other HCL: `#`, `//` and `/* */` comments, `"…${x}…"`, heredocs, `name = value`.
static HCL: Syntax = Syntax {
    flavor: Flavor::Hcl,
    line: &[b"#", b"//"],
    quotes: b"\"",
    kw: &[
        "check", "data", "dynamic", "import", "locals", "module", "moved", "output", "provider", "removed", "resource",
        "terraform", "variable",
    ],
    ctl: &["else", "endfor", "endif", "for", "if", "in"],
    types: &["any", "bool", "list", "map", "number", "object", "set", "string", "tuple"],
    lits: &["false", "null", "true"],
    vars: &["count", "each", "local", "path", "self", "var"],
    ..BASE
};

/// CMake: commands (in any case) with their arguments, `#[[bracket comments]]`, `[[bracket arguments]]`, `${VAR}`,
/// `$<generator:expressions>`; the uppercase keywords are colored as written.
static CMAKE: Syntax = Syntax {
    flavor: Flavor::CMake,
    line: &[b"#"],
    block: None,
    quotes: b"\"",
    multi: b"\"",
    kw: &[
        "AND", "APPEND", "BOOL", "CACHE", "COMMAND", "COMPONENTS", "CONFIGURE_DEPENDS", "DEFINED", "DEPENDS",
        "DESTINATION", "DIRECTORY", "EQUAL", "EXISTS", "FATAL_ERROR", "FILES", "FORCE", "GLOB", "GLOB_RECURSE",
        "GREATER", "INTERFACE", "IN_LIST", "LESS", "MATCHES", "NAMES", "NOT", "OPTIONAL", "OR", "OUTPUT",
        "PARENT_SCOPE", "PATH", "PATHS", "PRIVATE", "PROPERTIES", "PUBLIC", "QUIET", "REQUIRED", "SEND_ERROR",
        "SOURCES", "STATUS", "STREQUAL", "STRING", "TARGET", "TARGETS", "TYPE", "VERSION", "VERSION_GREATER",
        "VERSION_LESS", "WARNING", "WORKING_DIRECTORY",
    ],
    ctl: &[
        "block", "break", "continue", "else", "elseif", "endblock", "endforeach", "endfunction", "endif", "endmacro",
        "endwhile", "foreach", "function", "if", "macro", "return", "while",
    ],
    lits: &["FALSE", "NO", "OFF", "ON", "TRUE", "YES"],
    sigils: b"$",
    resync: true,
    ..BASE
};

pub(super) fn syntax(lang: Lang) -> &'static Syntax {
    match lang {
        Lang::C => &C,
        Lang::Cpp => &CPP,
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
        Lang::Vb => &VB,
        Lang::AutoHotkey => &AHK,
        Lang::Perl => &PERL,
        Lang::R => &R,
        Lang::Hcl => &HCL,
        Lang::CMake => &CMAKE,
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
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 || (sx.dollar_ident && c == b'$') || (sx.flavor == Flavor::R && c == b'.')
}

/// (R names may hold dots: `is.na`, `data.frame`.)
fn ident_char(sx: &Syntax, c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 || (sx.dollar_ident && c == b'$') || (sx.flavor == Flavor::R && c == b'.')
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

fn number_end(sx: &Syntax, t: &[u8], i: usize) -> usize {
    let hex = t[i] == b'0' && matches!(at(t, i + 1), b'x' | b'X');
    let mut j = i + 1;
    while j < t.len() {
        let b = t[j];
        let more = b.is_ascii_alphanumeric()
            || b == b'_'
            || (b == b'.' && at(t, j + 1).is_ascii_digit())
            || (matches!(b, b'+' | b'-') && !hex && matches!(t[j - 1], b'e' | b'E'))
            // 1'000'000, 0xFF'FF
            || (b == b'\'' && sx.digit_sep && t[j - 1].is_ascii_alphanumeric() && at(t, j + 1).is_ascii_alphanumeric());
        if !more {
            break;
        }
        j += 1;
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

/// A string from `s`, its body scanned from `i`. `mark` is the quote as kept in the state (`ESC_QUOTE` for `$'…'`
/// and `E'…'`); `b` is the state's (the escape pending, and with `resync` what `guarded_rest` keeps).
#[allow(clippy::too_many_arguments)]
fn string_rest(sx: &Syntax, t: &[u8], s: usize, i: usize, mark: u8, b: u16, st: State, o: &mut Out) -> Step {
    let (q, esc) = match mark {
        ESC_QUOTE => (b'\'', b'\\'),
        q if sx.raw.contains(&q) => (q, 0),
        q if sx.flavor == Flavor::Sql => (q, if st.mode & MYSQL != 0 { b'\\' } else { 0 }),
        q => (q, sx.esc),
    };
    let multi = sx.multi.contains(&q);
    if multi && sx.resync {
        return guarded_rest(t, s, i, mark, q, esc, b, st, o);
    }
    let (e, closed, pending) = scan_str(t, i, q, esc, b & 1 != 0, !multi);
    o.put(s, e, Tok::Str);
    if closed { Ok(e) } else { Err(State { kind: STR, a: mark, b: pending as u16, ..st }) }
}

/// A string that may span lines, but gives up (ends, as far as coloring goes) at the end of a blank line or after
/// `MAX_STR_LINES` lines: in shell scripts, SQL dumps or PHP pages a quote is easily a stray one (an apostrophe in
/// text, `It\'s`), and it must not recolor the rest of the file. It only looks at what it has read, so it ends
/// the same wherever the text was cut. `b` (and the state's): bit 0 an escape pending, bit 1 the line so far is
/// blank, then the lines so far.
#[allow(clippy::too_many_arguments)]
fn guarded_rest(t: &[u8], s: usize, mut i: usize, mark: u8, q: u8, esc: u8, b: u16, st: State, o: &mut Out) -> Step {
    let n = t.len();
    let (mut pending, mut blank, mut lines) = (b & 1 != 0, b & 2 != 0, b >> 2);
    while i < n {
        let c = t[i];
        if c == b'\n' {
            pending = false;
            lines = lines.saturating_add(1);
            if blank || lines >= MAX_STR_LINES {
                o.put(s, i, Tok::Str);
                return Ok(i);
            }
            blank = true;
        } else {
            if !c.is_ascii_whitespace() {
                blank = false;
            }
            if pending {
                pending = false;
            } else if c == esc && esc != 0 {
                pending = true;
            } else if c == q {
                o.put(s, i + 1, Tok::Str);
                return Ok(i + 1);
            }
        }
        i += 1;
    }
    o.put(s, n, Tok::Str);
    Err(State { kind: STR, a: mark, b: pending as u16 | (blank as u16) << 1 | lines.min(0x3FFF) << 2, ..st })
}

/// A `/regex/` from `s`, scanned from `i`; `class`: inside `[…]`. At a line break it is over (the caller decides
/// before whether a `/` starts one at all).
fn regex_rest(t: &[u8], s: usize, mut i: usize, mut class: bool, mut pending: bool, st: State, o: &mut Out) -> Step {
    while i < t.len() {
        let b = t[i];
        if pending {
            pending = false;
        } else if b == b'\\' {
            pending = true;
        } else if b == b'\n' {
            o.put(s, i, Tok::Str);
            return Ok(i);
        } else if b == b'[' {
            class = true;
        } else if b == b']' {
            class = false;
        } else if b == b'/' && !class {
            let e = i + 1 + t[i + 1..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            o.put(s, e, Tok::Str);
            return Ok(e);
        }
        i += 1;
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: REGEX, a: class as u8 | (pending as u8) << 1, b: 0, ..st })
}

/// The hash of a heredoc's end word, as kept in its state (22 bits: `b` and the top of `a`).
fn word_hash(w: &[u8]) -> (u8, u16) {
    let h = w.iter().fold(0x811C_9DC5u32, |h, &c| (h ^ c as u32).wrapping_mul(0x0100_0193));
    (((h >> 16) as u8 & 0x3F) << 2, h as u16)
}

/// `<<EOF`, `<<-'EOF'`, `<<"END"` (shell) or `<<~SQL` (Ruby) at `i`: (where the opener ends, the end word, whether
/// that may be indented).
fn heredoc_open<'t>(sx: &Syntax, t: &'t [u8], i: usize) -> Option<(usize, &'t [u8], bool)> {
    if !t[i..].starts_with(b"<<") || at(t, i + 2) == b'<' {
        return None;
    }
    let mut j = i + 2;
    let indent = matches!(at(t, j), b'-' | b'~');
    if indent {
        j += 1;
    }
    if sx.flavor == Flavor::Shell {
        // `cat << EOF` is fine in a shell; `$(( x << y ))` is a shift (looked for in the 256 bytes before)
        j += t[j..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        let from = i.saturating_sub(256);
        let line = &t[memchr::memrchr(b'\n', &t[from..i]).map_or(from, |p| from + p + 1)..i];
        if memchr::memmem::find(line, b"((").is_some() {
            return None;
        }
        if at(t, j) == b'\\' {
            j += 1;
        }
    }
    let q = at(t, j);
    if matches!(q, b'\'' | b'"' | b'`') {
        let len = t[j + 1..].iter().take(64).position(|&b| b == q || b == b'\n')?;
        return (at(t, j + 1 + len) == q && len > 0).then(|| (j + len + 2, &t[j + 1..j + 1 + len], indent));
    }
    // a bare word starts with a capital (EOF, END, SQL...), which `x << y` and `list << item` rarely do
    if !(q.is_ascii_uppercase() || q == b'_') {
        return None;
    }
    let len = t[j..].iter().take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_').count();
    Some((j + len, &t[j..j + len], indent))
}

/// A heredoc's lines from `i` (a line start) up to and including its end line. The text's end counts as a line's
/// end (a segment is its line without the line break).
fn heredoc_rest(t: &[u8], mut i: usize, a: u8, b: u16, st: State, o: &mut Out) -> Step {
    loop {
        let e = line_end(t, i);
        let mut w = &t[i..e];
        w = w.strip_suffix(b"\r").unwrap_or(w);
        if a & 1 != 0 {
            w = w.trim_ascii_start();
        }
        o.put(i, e, Tok::Str);
        if word_hash(w) == (a & !3, b) {
            return Ok(e);
        }
        if e >= t.len() {
            break;
        }
        i = e + 1;
    }
    Err(State { kind: HEREDOC, a: a & !2, b, ..st })
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

/// Perl's POD documentation from `i` up to and including its `=cut` line; `line_start`: whether `i` starts a line.
fn pod_rest(t: &[u8], mut i: usize, mut line_start: bool, st: State, o: &mut Out) -> Step {
    loop {
        let e = line_end(t, i);
        o.put(i, e, Tok::Comment);
        if line_start && t[i..e].starts_with(b"=cut") && matches!(at(t, i + 4), b' ' | b'\t' | b'\r' | b'\n' | 0) {
            return Ok(e);
        }
        if e >= t.len() {
            break;
        }
        i = e + 1;
        line_start = true;
    }
    Err(State { kind: POD, a: 0, b: 0, ..st })
}

/// The closing delimiter for an opening one (brackets pair up; anything else closes itself).
fn closing(open: u8) -> u8 {
    match open {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        c => c,
    }
}

/// A Perl quote-like operator at `i` (`q`, `qq`, `qw`, `qr`, `qx`, `m`, `s`, `tr`, `y` right before its delimiter):
/// where its body starts, the closing delimiter, and whether it has two parts (`s/a/b/`).
fn perl_quote_open(t: &[u8], i: usize) -> Option<(usize, u8, bool)> {
    if i > 0 && (t[i - 1].is_ascii_alphanumeric() || matches!(t[i - 1], b'_' | b'$' | b'@' | b'%' | b'&' | b'*' | b'-' | b'>' | b':')) {
        return None;
    }
    let e = i + t[i..].iter().take(3).take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
    let two = match &t[i..e] {
        b"q" | b"qq" | b"qw" | b"qr" | b"qx" | b"m" => false,
        b"s" | b"tr" | b"y" => true,
        _ => return None,
    };
    // the delimiter right after the word (`s => 1`, `y = 2`, `qw;` and `q,` aren't quotes)
    let d = at(t, e);
    if d == 0 || d.is_ascii_alphanumeric() || d.is_ascii_whitespace() || matches!(d, b'_' | b'=' | b',' | b';' | b')' | b']' | b'}' | b'>' | b'-') {
        return None;
    }
    Some((e + 1, closing(d), two))
}

/// The rest of a Perl quote-like construct from `i` (see `QUOTE_LIKE` for `close` and `b`).
fn quote_like_rest(t: &[u8], s: usize, mut i: usize, mut close: u8, b: u16, st: State, o: &mut Out) -> Step {
    let n = t.len();
    let (mut depth, mut more, mut esc, mut lines) = (b & 0x3F, b & 0x40 != 0, b & 0x80 != 0, (b >> 8) as u8);
    let set_open = |close: u8| match close {
        b')' => b'(',
        b']' => b'[',
        b'}' => b'{',
        b'>' => b'<',
        _ => 0,
    };
    let mut open = set_open(close);
    while i < n {
        let c = t[i];
        if c == b'\n' {
            lines = lines.saturating_add(1);
            if lines >= MAX_QUOTE_LINES {
                o.put(s, i, Tok::Str);
                return Ok(i);
            }
        }
        if close == 0 {
            // between the parts of s{…}{…}: whitespace, then the second part's own delimiter
            if !c.is_ascii_whitespace() {
                close = closing(c);
                open = set_open(close);
                depth = 0;
                more = false;
            }
            i += 1;
            continue;
        }
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == open && open != 0 {
            depth = (depth + 1).min(0x3F);
        } else if c == close {
            if depth > 0 {
                depth -= 1;
            } else if more {
                more = false;
                if open != 0 {
                    close = 0;
                }
            } else {
                // modifiers: s/a/b/gi
                let e = i + 1 + t[i + 1..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
                o.put(s, e, Tok::Str);
                return Ok(e);
            }
        }
        i += 1;
    }
    o.put(s, n, Tok::Str);
    let b = depth | (more as u16) << 6 | (esc as u16) << 7 | (lines as u16) << 8;
    Err(State { kind: QUOTE_LIKE, a: close, b, ..st })
}

/// A C++ raw string `R"delim(` at `i` (also `u8R"`, `LR"`…): where its body starts and the hash of `delim`.
fn cpp_raw_open(t: &[u8], i: usize) -> Option<(usize, (u8, u16))> {
    if i > 0 && (t[i - 1].is_ascii_alphanumeric() || t[i - 1] == b'_') {
        return None;
    }
    let r = [&b"R\""[..], b"u8R\"", b"uR\"", b"UR\"", b"LR\""].into_iter().find(|p| t[i..].starts_with(p))?.len();
    let d = t[i + r..].iter().take(17).take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
    (d <= 16 && at(t, i + r + d) == b'(').then(|| (i + r + d + 1, word_hash(&t[i + r..i + r + d])))
}

/// The rest of a C++ raw string: up to `)delim"`, `delim` given by its hash.
fn cpp_raw_rest(t: &[u8], s: usize, mut i: usize, (a, b): (u8, u16), st: State, o: &mut Out) -> Step {
    while let Some(p) = memchr::memchr(b')', &t[i..]) {
        i += p + 1;
        let d = t[i..].iter().take(17).take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
        if at(t, i + d) == b'"' && word_hash(&t[i..i + d]) == (a, b) {
            o.put(s, i + d + 1, Tok::Str);
            return Ok(i + d + 1);
        }
    }
    o.put(s, t.len(), Tok::Str);
    Err(State { kind: CPP_RAW, a, b, ..st })
}

/// The rest of an interpolated string (see `INTERP`): a `"` inside a `{…}` hole starts a string of its own instead
/// of ending this one (`$"{(ok ? "yes" : "no")}"`). Over at the line's end, closed or not.
#[allow(clippy::too_many_arguments)]
fn interp_rest(sx: &Syntax, t: &[u8], s: usize, mut i: usize, a: u8, mut depth: u16, st: State, o: &mut Out) -> Step {
    let hcl = sx.flavor == Flavor::Hcl;
    let (mut esc, mut hole, mut inner, mut inner_esc) = (a & 1 != 0, a & 2 != 0, a & 4 != 0, a & 8 != 0);
    while i < t.len() {
        let c = t[i];
        if c == b'\n' {
            o.put(s, i, Tok::Str);
            return Ok(i);
        }
        if inner {
            if inner_esc {
                inner_esc = false;
            } else if c == b'\\' {
                inner_esc = true;
            } else if c == b'"' {
                inner = false;
            }
        } else if hole {
            match c {
                b'"' => inner = true,
                b'{' => depth = depth.saturating_add(1),
                b'}' if depth == 0 => hole = false,
                b'}' => depth -= 1,
                _ => {}
            }
        } else if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'"' {
            o.put(s, i + 1, Tok::Str);
            return Ok(i + 1);
        } else if hcl && matches!(c, b'$' | b'%') && at(t, i + 1) == c && at(t, i + 2) == b'{' {
            // `$${` is a literal `${`
            i += 3;
            continue;
        } else if hcl && matches!(c, b'$' | b'%') && at(t, i + 1) == b'{' {
            (hole, depth) = (true, 0);
            i += 2;
            continue;
        } else if !hcl && c == b'{' {
            if at(t, i + 1) == b'{' {
                // `{{` is a brace
                i += 2;
                continue;
            }
            (hole, depth) = (true, 0);
        }
        i += 1;
    }
    o.put(s, t.len(), Tok::Str);
    let a = esc as u8 | (hole as u8) << 1 | (inner as u8) << 2 | (inner_esc as u8) << 3;
    Err(State { kind: INTERP, a, b: depth, ..st })
}

/// C#'s """raw strings""" have no escapes; other languages' triple-quoted strings do.
fn triple_esc(sx: &Syntax) -> u8 {
    if sx.flavor == Flavor::CSharp { 0 } else { sx.esc }
}

/// Continues the token the text starts inside.
fn resume(sx: &Syntax, t: &[u8], st: State, o: &mut Out) -> Step {
    match st.kind {
        LINE_COMMENT => {
            let e = comment_end(sx, t, 0);
            o.put(0, e, Tok::Comment);
            if e == t.len() { Err(st) } else { Ok(e) }
        }
        BLOCK_COMMENT => block_rest(sx, t, 0, 0, st.a.max(1), st, o),
        STR => string_rest(sx, t, 0, 0, st.a, st.b, st, o),
        TRIPLE => triple_rest(t, 0, 0, st.a, triple_esc(sx), st.b == 1, st, o),
        RAW => raw_rest(t, 0, 0, st.a, st, o),
        VERBATIM => verbatim_rest(t, 0, 0, st, o),
        HERE => here_rest(t, 0, 0, st.a, st.col0, st, o),
        LONG => long_rest(t, 0, 0, st.a, st.b == 1, st, o),
        REGEX => regex_rest(t, 0, 0, st.a & 1 != 0, st.a & 2 != 0, st, o),
        HEREDOC if st.col0 => heredoc_rest(t, 0, st.a, st.b, st, o),
        HEREDOC => {
            // the rest of a heredoc line, then its next lines
            let e = line_end(t, 0);
            o.put(0, e, Tok::Str);
            if e == t.len() { Err(st) } else { heredoc_rest(t, e + 1, st.a, st.b, st, o) }
        }
        POD => pod_rest(t, 0, st.col0, st, o),
        DATA => {
            o.put(0, t.len(), Tok::Comment);
            Err(st)
        }
        QUOTE_LIKE => quote_like_rest(t, 0, 0, st.a, st.b, st, o),
        CPP_RAW => cpp_raw_rest(t, 0, 0, (st.a, st.b), st, o),
        INTERP => interp_rest(sx, t, 0, 0, st.a, st.b, st, o),
        _ => Ok(0),
    }
}

/// Where a line comment starting at `i` ends: the line's end (or in PHP a `?>` before it, which ends PHP code).
fn comment_end(sx: &Syntax, t: &[u8], i: usize) -> usize {
    let e = line_end(t, i);
    if sx.flavor == Flavor::Php {
        if let Some(p) = memchr::memmem::find(&t[i..e], b"?>") {
            return i + p;
        }
    }
    e
}

fn line_comment(sx: &Syntax, t: &[u8], i: usize) -> bool {
    sx.line.iter().any(|lc| {
        t[i..].starts_with(lc)
            // shells: `#` starts a comment only at the start of a word (not in `a#b` or `${#x}`)
            && !(sx.flavor == Flavor::Shell && i > 0 && !matches!(t[i - 1], b' ' | b'\t' | b'\n' | b'\r' | b';' | b'|' | b'&' | b'('))
            // AutoHotkey: `;` after a space or at the line's start (`Send a;b` sends "a;b")
            && !(sx.flavor == Flavor::Ahk && i > 0 && !matches!(t[i - 1], b' ' | b'\t' | b'\n' | b'\r'))
            // PHP 8 attributes: #[Route('/x')]
            && !(sx.flavor == Flavor::Php && t[i..].starts_with(b"#["))
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
    if sx.flavor == Flavor::Perl && t[i] == b'$' {
        // $#array (the last index, not a comment), $_, $1, $', $", $/ and the other punctuation variables
        if c == b'#' && at(t, j + 1) == b'$' {
            return Some(j + 1);
        } else if c == b'#' && (ident_start(sx, at(t, j + 1)) || at(t, j + 1) == b'{') {
            j += 1;
        } else if c == b'^' && at(t, j + 1).is_ascii_uppercase() {
            return Some(j + 2);
        } else if c.is_ascii_digit() {
            return Some(j + t[j..].iter().take_while(|b| b.is_ascii_digit()).count());
        } else if matches!(c, b'&' | b'`' | b'\'' | b'"' | b'+' | b'!' | b'@' | b'/' | b'\\' | b',' | b';' | b'.' | b'0' | b'<' | b'>' | b'$') {
            return Some(j + 1);
        }
    }
    let c = at(t, j);
    if sx.flavor == Flavor::CMake && c == b'<' {
        // a generator expression, $<TARGET_FILE:app> (nested ones too; not past a quote, which starts a string)
        let mut depth = 0;
        for (k, &b) in t[j..t.len().min(j + 256)].iter().enumerate() {
            match b {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j + k + 1);
                    }
                }
                b'"' | b'\n' => return None,
                _ => {}
            }
        }
        return None;
    }
    if c == b'{' && sx.flavor == Flavor::Php {
        // `${name}` (nothing more: `${ … ?>` must not hide the end of the PHP code)
        let e = j + 1 + t[j + 1..].iter().take(64).take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
        return (at(t, e) == b'}').then_some(e + 1);
    }
    if c == b'{' {
        // `${name}` (looked for in the next 64 bytes only: `${${${…` must not cost a scan each)
        let e = memchr::memchr2(b'}', b'\n', &t[j..t.len().min(j + 64)])?;
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
    // CMake: $ENV{PATH}, $CACHE{X}
    if sx.flavor == Flavor::CMake && at(t, e) == b'{' {
        let k = t[e + 1..].iter().take(64).take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')).count();
        if at(t, e + 1 + k) == b'}' {
            e += k + 2;
        }
    }
    Some(e)
}

/// An AutoHotkey hotkey (`^!s::`, `#n::`, `~LButton & RButton::`), hotstring (`:*:btw::`) or label (`Start:`) at
/// the start of a line (`i`): where it ends, and whether what follows is a hotstring's replacement text.
fn ahk_label(t: &[u8], i: usize) -> Option<(usize, bool)> {
    let l = &t[i..line_end(t, i)];
    if l.starts_with(b";") || l.starts_with(b"/*") {
        return None;
    }
    if l.first() == Some(&b':') {
        let opts = 1 + l[1..].iter().take(16).position(|&b| b == b':')?;
        let p = opts + 1 + memchr::memmem::find(&l[opts + 1..], b"::")?;
        return Some((i + p + 2, true));
    }
    let Some(p) = memchr::memmem::find(l, b"::") else {
        // Label:
        let w = l.iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
        let rest = l.get(w + 1..).unwrap_or(b"").trim_ascii();
        return (w > 0 && at(l, w) == b':' && (rest.is_empty() || rest.starts_with(b";"))).then_some((i + w + 1, false));
    };
    let key = |w: &[u8]| w.iter().all(|&b| b > b' ' && b < 0x7F && !matches!(b, b'"' | b'\'' | b'(' | b')' | b'=' | b',' | b'%'));
    let words: Vec<&[u8]> = l[..p].split(|&b| b == b' ' || b == b'\t').filter(|w| !w.is_empty()).collect();
    let ok = match words.as_slice() {
        [k] => key(k),
        [a, b"&", b] => key(a) && key(b),
        [k, up] => key(k) && up.eq_ignore_ascii_case(b"up"),
        _ => false,
    };
    (ok && p <= 40).then_some((i + p + 2, false))
}

/// Per-language extras at `i` that come before the general rules (`line_start`: only whitespace before it on its
/// line; `regex_ok` as in `code_run`, for what follows). Some(step) when one applied.
fn extras(sx: &Syntax, t: &[u8], i: usize, line_start: bool, regex_ok: &mut bool, st: State, o: &mut Out) -> Option<Step> {
    let c = t[i];
    let n = t.len();
    match sx.flavor {
        Flavor::Cpp => {
            if matches!(c, b'R' | b'u' | b'U' | b'L') {
                if let Some((from, h)) = cpp_raw_open(t, i) {
                    return Some(cpp_raw_rest(t, i, from, h, st, o));
                }
            }
        }
        Flavor::Vb => {
            // &HFF, &O17, &B101
            if c == b'&' && matches!(at(t, i + 1), b'h' | b'H' | b'o' | b'O' | b'b' | b'B') && at(t, i + 2).is_ascii_hexdigit() {
                let e = i + 2 + t[i + 2..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
                let e = e + matches!(at(t, e), b'&' | b'%') as usize;
                o.put(i, e, Tok::Num);
                return Some(Ok(e));
            }
            // #10/8/2026# dates
            if c == b'#' && at(t, i + 1).is_ascii_digit() {
                if let Some(p) = t[i + 1..n.min(i + 40)].iter().position(|&b| b == b'#' || b == b'\n').filter(|&p| t[i + 1 + p] == b'#') {
                    o.put(i, i + p + 2, Tok::Num);
                    return Some(Ok(i + p + 2));
                }
            }
        }
        Flavor::Ahk => {
            if line_start {
                if let Some((e, hotstring)) = ahk_label(t, i) {
                    o.put(i, e, Tok::Section);
                    if hotstring {
                        let le = line_end(t, e);
                        o.put(e, le, Tok::Str);
                        return Some(Ok(le));
                    }
                    return Some(Ok(e));
                }
            }
            // %var%
            if c == b'%' {
                let e = i + 1 + t[i + 1..].iter().take(64).take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b >= 0x80).count();
                if e > i + 1 && at(t, e) == b'%' {
                    o.put(i, e + 1, Tok::Var);
                    return Some(Ok(e + 1));
                }
            }
            // version 2 quotes with ' too
            if c == b'\'' && st.mode & AHK_V2 != 0 {
                return Some(string_rest(sx, t, i, i + 1, b'\'', 0, st, o));
            }
        }
        Flavor::Perl => {
            let col0 = if i == 0 { st.col0 } else { t[i - 1] == b'\n' };
            // =head1 … =cut (where a statement starts after it, as when it's continued from the text before)
            if col0 && c == b'=' && at(t, i + 1).is_ascii_alphabetic() {
                *regex_ok = true;
                return Some(pod_rest(t, i, true, st, o));
            }
            if col0 && c == b'_' {
                for w in [&b"__END__"[..], b"__DATA__"] {
                    if t[i..].starts_with(w) && !ident_char(sx, at(t, i + w.len())) {
                        o.put(i, n, Tok::Comment);
                        return Some(Err(State { kind: DATA, ..st }));
                    }
                }
            }
            if matches!(c, b'q' | b'm' | b's' | b't' | b'y') {
                if let Some((from, close, two)) = perl_quote_open(t, i) {
                    *regex_ok = false;
                    o.put(i, from - 1, Tok::Keyword);
                    return Some(quote_like_rest(t, from - 1, from, close, (two as u16) << 6, st, o));
                }
            }
        }
        Flavor::R => {
            // %in%, %>%, %%
            if c == b'%' {
                let e = i + 1 + t[i + 1..].iter().take(16).take_while(|b| !matches!(b, b'%' | b'\n' | b' ' | b'"' | b'\'' | b'`' | b'#')).count();
                if at(t, e) == b'%' {
                    o.put(i, e + 1, Tok::Keyword);
                    return Some(Ok(e + 1));
                }
            }
        }
        Flavor::Hcl => {
            if c == b'"' {
                return Some(interp_rest(sx, t, i, i + 1, 0, 0, st, o));
            }
        }
        Flavor::CMake => {
            // #[[bracket comments]], [=[bracket arguments]=]
            if c == b'#' {
                if let Some((level, len)) = long_open(t, i + 1) {
                    return Some(long_rest(t, i, i + 1 + len, level, true, st, o));
                }
            }
            if let Some((level, len)) = long_open(t, i) {
                return Some(long_rest(t, i, i + len, level, false, st, o));
            }
        }
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
                    // '\n', '\'', '\u{1F600}' (never past the line's end)
                    if matches!(at(t, i + 2), b'\n' | 0) {
                        o.put(i, i + 2, Tok::Str);
                        return Some(Ok(i + 2));
                    }
                    let from = (i + 3).min(n);
                    let e = t[from..]
                        .iter()
                        .take(10)
                        .position(|&b| b == b'\'' || b == b'\n')
                        .filter(|&p| t[from + p] == b'\'')
                        .map_or(from, |p| from + p + 1);
                    o.put(i, e, Tok::Str);
                    return Some(Ok(e));
                }
                let len = crate::core::text::char_len_at(&t[i + 1..]).max(1);
                if at(t, i + 1 + len) == b'\'' && at(t, i + 1) != b'\n' {
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
                return Some(interp_rest(sx, t, i, i + 2, 0, 0, st, o));
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
            if c == b'@' && matches!(at(t, i + 1), b'"' | b'\'') {
                let k = i + 2 + t[i + 2..].iter().take_while(|&&b| matches!(b, b' ' | b'\t' | b'\r' | 0x0C)).count();
                if k == n || t[k] == b'\n' {
                    return Some(here_rest(t, i, i + 2, t[i + 1], false, st, o));
                }
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
                        // (generic types nest a little; `[a [a [a …` mustn't be looked through again and again)
                        b'[' if depth >= 3 => break,
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
            for tag in [&b"<?php"[..], b"<?="] {
                if t[i..].starts_with(tag) {
                    o.put(i, i + tag.len(), Tok::Control);
                    return Some(Ok(i + tag.len()));
                }
            }
        }
        Flavor::Sql => {
            let mysql = st.mode & MYSQL != 0;
            // E'it\'s' (PostgreSQL), and in MySQL "strings" with \ escapes
            if matches!(c, b'E' | b'e') && at(t, i + 1) == b'\'' && (i == 0 || !ident_char(sx, t[i - 1])) {
                return Some(string_rest(sx, t, i, i + 2, ESC_QUOTE, 0, st, o));
            }
            if c == b'"' && mysql {
                return Some(string_rest(sx, t, i, i + 1, b'"', 0, st, o));
            }
            // "quoted", `quoted` and [bracketed] names
            if c == b'"' || c == b'`' {
                let (e, _, _) = scan_str(t, i + 1, c, 0, false, true);
                o.put(i, e, Tok::Var);
                return Some(Ok(e));
            }
            if c == b'[' {
                // (a name is short: not a scan to the line's end at each `[`)
                if let Some(p) = t[i + 1..n.min(i + 129)].iter().position(|&b| b == b']' || b == b'[' || b == b'\n') {
                    if t[i + 1 + p] == b']' && p > 0 {
                        o.put(i, i + p + 2, Tok::Var);
                        return Some(Ok(i + p + 2));
                    }
                }
            }
        }
        Flavor::Shell => {
            // $'tab\there\'s' (backslash escapes in single quotes)
            if c == b'$' && at(t, i + 1) == b'\'' {
                return Some(string_rest(sx, t, i, i + 2, ESC_QUOTE, 0, st, o));
            }
        }
        Flavor::Plain | Flavor::Batch => {}
    }
    None
}

/// After these words a `/` starts a regex rather than dividing (JavaScript; Perl).
const REGEX_AFTER: [&[u8]; 14] =
    [b"await", b"case", b"delete", b"do", b"else", b"in", b"instanceof", b"new", b"of", b"return", b"throw", b"typeof", b"void", b"yield"];
const PERL_REGEX_AFTER: [&[u8]; 14] =
    [b"and", b"grep", b"if", b"join", b"map", b"not", b"or", b"push", b"return", b"split", b"unless", b"until", b"when", b"while"];

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

pub(super) fn code(sx: &Syntax, t: &[u8], st: State, o: &mut Out) -> State {
    code_run(sx, t, st, o, &mut None)
}

/// PHP code (inside `<?php … ?>`): the state after it, and where the code ends when its `?>` is in `t` (after
/// the `?>`).
pub(super) fn php_code(t: &[u8], st: State, o: &mut Out) -> (State, Option<usize>) {
    let mut close = None;
    let s = code_run(&PHP, t, st, o, &mut close);
    (s, close)
}

fn code_run(sx: &Syntax, t: &[u8], st: State, o: &mut Out, close: &mut Option<usize>) -> State {
    let n = t.len();
    let mut i = 0;
    // JavaScript, Perl: a `/` here starts a regex (a value may come) rather than dividing; kept in `a` (1:
    // dividing). After a comment, POD or a heredoc's lines it may; after a string or a regex it doesn't.
    let mut regex_ok = match st.kind {
        0 => st.a == 0,
        LINE_COMMENT | BLOCK_COMMENT | POD | HEREDOC => true,
        _ => false,
    };
    // A heredoc opened on this line: its lines start after it (its state's `a` and `b`), unless something else
    // (a string, a comment) goes on past the line's end.
    let mut heredoc: Option<(u8, u16)> = None;
    let mut heredoc_line_end = 0;
    if st.kind == HEREDOC && st.a & 2 != 0 {
        heredoc = Some((st.a & !2, st.b));
        heredoc_line_end = line_end(t, 0);
    } else if st.kind != 0 {
        match resume(sx, t, st, o) {
            Ok(e) => i = e,
            Err(s) => return s,
        }
    }
    let mut mid = State { kind: 0, a: 0, b: 0, ..st };
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
        if heredoc.is_some() && i > heredoc_line_end {
            heredoc = None;
        }
        let c = t[i];
        match c {
            b'\n' => {
                (bol, stmt, echo, expect) = (true, true, false, 0);
                i += 1;
                if let Some((a, b)) = heredoc.take() {
                    regex_ok = true;
                    step!(heredoc_rest(t, i, a, b, mid, o));
                }
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
        if sx.flavor == Flavor::Php && t[i..].starts_with(b"?>") {
            // the end of the PHP code (the page around it is HTML)
            o.put(i, i + 2, Tok::Control);
            *close = Some(i + 2);
            return mid;
        }
        if (sx.flavor == Flavor::Shell && c == b'\\') || (matches!(sx.flavor, Flavor::PowerShell | Flavor::Ahk) && c == b'`') {
            // an escaped character outside quotes (`It\'s`), or a line continuation
            i += if matches!(at(t, i + 1), b'\n' | 0) { 1 } else { 2 };
            continue;
        }
        if sx.flavor == Flavor::Vb && c == b':' {
            // a new statement (where `Rem` starts a comment)
            stmt = true;
            i += 1;
            continue;
        }
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
        if matches!(sx.flavor, Flavor::Shell | Flavor::Ruby | Flavor::Perl | Flavor::Hcl) && c == b'<' {
            if let Some((e, word, indent)) = heredoc_open(sx, t, i) {
                o.put(i, e, Tok::Str);
                let (a, b) = word_hash(word);
                heredoc = Some((a | indent as u8, b));
                // (once per line: `<<A <<B <<C …` mustn't look for the line's end each time)
                if e > heredoc_line_end {
                    heredoc_line_end = line_end(t, e);
                }
                i = e;
                continue;
            }
        }
        if sx.flavor == Flavor::Sql && mid.mode & MYSQL == 0 && (c == b'`' || t[i..].starts_with(b"/*!")) {
            mid.mode |= MYSQL;
        }
        if let Some(r) = extras(sx, t, i, line_start, &mut regex_ok, mid, o) {
            step!(r);
        }
        // MySQL's # comments (and `# note` at a line's start in any SQL; `#temp` is a T-SQL table)
        let sql_hash = sx.flavor == Flavor::Sql
            && c == b'#'
            && (mid.mode & MYSQL != 0 || (line_start && matches!(at(t, i + 1), b' ' | b'\t' | b'\r' | b'\n' | 0)));
        if sql_hash || line_comment(sx, t, i) {
            let e = comment_end(sx, t, i);
            o.put(i, e, Tok::Comment);
            if sx.flavor == Flavor::Sql && (contains_ci(&t[i..e], b"mysql") || contains_ci(&t[i..e], b"mariadb")) {
                mid.mode |= MYSQL;
            }
            regex_ok = true;
            if e == n {
                return State { kind: LINE_COMMENT, ..mid };
            }
            i = e;
            continue;
        }
        if let Some((open, _)) = sx.block {
            // (AutoHotkey's `/*` only at the start of a line)
            if t[i..].starts_with(open) && (sx.flavor != Flavor::Ahk || line_start) {
                regex_ok = true;
                step!(block_rest(sx, t, i, i + open.len(), 1, mid, o));
            }
        }
        if sx.regex && c == b'/' && regex_ok {
            // (judged only from what came before, so it's the same wherever a long line is cut)
            regex_ok = false;
            step!(regex_rest(t, i, i + 1, false, false, mid, o));
        }
        if sx.preproc && c == b'#' && line_start {
            // #include <x.h>, #define, #region
            let w = i + 1 + t[i + 1..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
            let e = w + t[w..].iter().take_while(|b| b.is_ascii_alphabetic()).count();
            if sx.flavor == Flavor::Cpp && w > i + 1 && at(t, e) == b'"' {
                // `# R"(…` is a raw string after a stray `#`, as when the text is cut after the space
                o.put(i, i + 1, Tok::Control);
                i += 1;
                continue;
            }
            o.put(i, e, Tok::Control);
            if sx.flavor == Flavor::Ahk && t[w..e].eq_ignore_ascii_case(b"requires") && contains_ci(&t[e..line_end(t, e)], b"v2") {
                mid.mode |= AHK_V2;
            }
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
            regex_ok = false;
            i = e;
            continue;
        }
        if sx.sigils.contains(&c) {
            if let Some(e) = sigil_var(sx, t, i) {
                o.put(i, e, Tok::Var);
                regex_ok = false;
                i = e;
                continue;
            }
        }
        if sx.triple.contains(&c) && at(t, i + 1) == c && at(t, i + 2) == c {
            regex_ok = false;
            step!(triple_rest(t, i, i + 3, c, triple_esc(sx), false, mid, o));
        }
        if sx.quotes.contains(&c) {
            regex_ok = false;
            step!(string_rest(sx, t, i, i + 1, c, 0, mid, o));
        }
        if c.is_ascii_digit() || (c == b'.' && at(t, i + 1).is_ascii_digit() && (i == 0 || !ident_char(sx, t[i - 1]))) {
            let e = number_end(sx, t, i);
            o.put(i, e, Tok::Num);
            expect = 0;
            regex_ok = false;
            i = e;
            continue;
        }
        if ident_start(sx, c) {
            let mut e = ident_end(sx, t, i);
            let w = &t[i..e];
            // after a '.', a keyword is a member name (`re.match`, `map.get`, `promise.catch`)
            let member = i > 0 && t[i - 1] == b'.' && !(i > 1 && t[i - 2] == b'.');
            let after: &[&[u8]] = if sx.flavor == Flavor::Perl { &PERL_REGEX_AFTER } else { &REGEX_AFTER };
            regex_ok = !member && after.contains(&w);
            if !o.on() && !matches!(sx.flavor, Flavor::Batch | Flavor::Vb) {
                i = e;
                continue;
            }
            let mut tok = if member { None } else { word_tok(sx, w) };
            if matches!(sx.flavor, Flavor::Batch | Flavor::Vb) {
                let ends = matches!(at(t, e), 0 | b' ' | b'\t' | b'\r' | b'\n');
                if stmt_start && ends && w.eq_ignore_ascii_case(b"rem") {
                    let le = line_end(t, i);
                    o.put(i, le, Tok::Comment);
                    i = le;
                    continue;
                }
                if sx.flavor == Flavor::Batch && w.eq_ignore_ascii_case(b"echo") {
                    echo = true;
                }
            }
            if tok.is_none() {
                let np = e + t[e..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
                let next = t.get(np).copied();
                tok = if expect == 1 {
                    Some(Tok::Func)
                } else if expect == 2 {
                    Some(Tok::Type)
                } else if sx.flavor == Flavor::Rust && at(t, e) == b'!' && at(t, e + 1) != b'=' {
                    e += 1;
                    Some(Tok::Func)
                } else if sx.dash_ident && w.contains(&b'-') {
                    Some(Tok::Func)
                } else if sx.flavor == Flavor::CSharp && next == Some(b'(') {
                    // C# methods are capitalized like types: `Console.WriteLine(…)`
                    Some(Tok::Func)
                } else if sx.flavor == Flavor::Ahk && w.len() > 2 && w[..2].eq_ignore_ascii_case(b"a_") {
                    // built-in variables: A_ScriptDir, A_Now
                    Some(Tok::Var)
                } else if sx.flavor == Flavor::Ahk && stmt_start && next == Some(b',') {
                    // a command in version 1's syntax: `MsgBox, Hello`
                    Some(Tok::Func)
                } else if sx.flavor == Flavor::Hcl && next == Some(b'=') && !matches!(at(t, np + 1), b'=' | b'>') {
                    // an argument: `ami = "…"`
                    Some(Tok::Attr)
                } else if sx.cap_types && w[0].is_ascii_uppercase() && w.iter().any(|b| b.is_ascii_lowercase()) {
                    Some(Tok::Type)
                } else if sx.calls && next == Some(b'(') {
                    Some(Tok::Func)
                } else {
                    None
                };
            }
            expect = 0;
            if tok == Some(Tok::Keyword) || (sx.flavor == Flavor::CMake && tok == Some(Tok::Control)) {
                let lower = w.to_ascii_lowercase();
                if FUNC_KW.contains(&lower.as_slice()) || (sx.flavor == Flavor::CMake && lower == b"macro") {
                    expect = 1;
                } else if TYPE_KW.contains(&lower.as_slice()) || (sx.flavor == Flavor::CSharp && lower == b"new") {
                    expect = 2;
                }
            }
            if let Some(tk) = tok {
                o.put(i, e, tk);
            }
            i = e;
            continue;
        }
        // punctuation: after `)` or `]` a `/` divides (after `<` it closes a JSX tag; in Perl after `}`, which more
        // often ends `$h{key}` than a block)
        regex_ok = !matches!(c, b')' | b']' | b'<') && !(sx.flavor == Flavor::Perl && c == b'}');
        expect = 0;
        i += 1;
    }
    if let Some((a, b)) = heredoc.filter(|_| i <= heredoc_line_end) {
        // the line that opens a heredoc goes on in the next text
        return State { kind: HEREDOC, a: a | 2, b, ..mid };
    }
    if sx.regex && !regex_ok {
        mid.a = 1;
    }
    mid
}

// ---- CSS (also SCSS and LESS) ----

const CSS_COMMENT: u8 = 1;
const CSS_STR: u8 = 2;
/// A `//` comment (SCSS, LESS) cut off by the end of a text.
const CSS_LINE_COMMENT: u8 = 3;

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
        CSS_LINE_COMMENT => {
            i = line_end(t, 0);
            o.put(0, i, Tok::Comment);
            if i == n {
                return st;
            }
        }
        _ => {}
    }
    // after a property outside braces (indented Sass), up to the line's end
    let mut value = false;
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
            b'/' if next == b'/' && (i == 0 || t[i - 1] != b':') => {
                let e = line_end(t, i);
                o.put(i, e, Tok::Comment);
                if e == n {
                    return State { kind: CSS_LINE_COMMENT, a: 0, b: depth, ..st };
                }
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
                value = false;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                value = false;
                i += 1;
            }
            b'\n' | b';' => {
                value = false;
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
                o.put(i, e, if depth > 0 || value { Tok::Num } else { Tok::Func });
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
                } else if depth == 0 && !value {
                    // a property outside braces is indented Sass's (`color: red`), or a media feature's
                    // (`(max-width: 600px)`); otherwise it's a selector
                    if at(t, e) == b':' && matches!(at(t, e + 1), b' ' | b'\t' | b'\r' | b'\n' | 0) {
                        o.put(i, e, Tok::Attr);
                        value = true;
                    } else {
                        o.put(i, e, Tok::Tag);
                    }
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
    use super::super::tests::{end_state, has, toks, view};
    use super::super::{Lang, State, Tok, lex};

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
        // a line comment at the start of a line, lexed together with the lines before it
        let st = end_state(Lang::Css, ".a {\n// closing } here\ncolor: red;\n");
        assert_eq!(st.b, 1);
        let st = end_state(Lang::Css, "@media print {\n  body {\n");
        assert!(toks(Lang::Css, "    color: red;", st).contains(&("color".into(), Tok::Attr)));
        assert!(toks(Lang::Css, "    color red", State::START).contains(&("color".into(), Tok::Tag)));
        // indented Sass: properties without braces
        let v = view(Lang::Css, ".btn\n  color: red\n  &:hover\n    background: #fff\n");
        assert!(line_has(&v[1], "color", Tok::Attr) && line_has(&v[1], "red", Tok::Str));
        assert!(line_has(&v[2], ":hover", Tok::Control) && line_has(&v[3], "#fff", Tok::Num));
        has(Lang::Css, "@media (max-width: 600px) {", &[("max-width", Tok::Attr), ("600px", Tok::Num)]);
    }

    fn line_has(line: &[(String, Tok)], s: &str, tok: Tok) -> bool {
        line.contains(&(s.to_string(), tok))
    }

    #[test]
    fn stray_quotes_stay_local() {
        // a heredoc is text up to its end word: its apostrophes start nothing
        let src = "cat <<EOF\nDon't run this as root.\nEOF\necho \"done\" # end\n";
        let v = view(Lang::Shell, src);
        assert_eq!(v[1], vec![("Don't run this as root.".into(), Tok::Str)]);
        assert!(line_has(&v[3], "\"done\"", Tok::Str) && line_has(&v[3], "# end", Tok::Comment));
        assert_eq!(end_state(Lang::Shell, src), State::START);
        let v = view(Lang::Shell, "cat <<-'END' | sort\n\tit's\n\tEND\nls # c\n");
        assert!(line_has(&v[0], "<<-'END'", Tok::Str) && line_has(&v[3], "# c", Tok::Comment));
        // `<<` in arithmetic is a shift
        assert!(line_has(&view(Lang::Shell, "echo $(( x << Y ))\necho 'a'\n")[1], "'a'", Tok::Str));
        // escapes outside quotes, and $'…' with escapes
        assert!(line_has(&view(Lang::Shell, "echo It\\'s done\nls # list\n")[1], "# list", Tok::Comment));
        assert!(line_has(&view(Lang::Shell, "printf $'it\\'s'\necho ok # c\n")[1], "# c", Tok::Comment));
        // a quote left open gives up at a blank line, or after 40 lines
        let v = view(Lang::Shell, "echo it's\nstill\n  \necho 'ok' # c\n");
        assert!(line_has(&v[1], "still", Tok::Str) && line_has(&v[3], "'ok'", Tok::Str) && line_has(&v[3], "# c", Tok::Comment));
        let v = view(Lang::Shell, &format!("x='{}\necho # c\n", "a\n".repeat(60)));
        assert!(line_has(&v[30], "a", Tok::Str) && v[45].is_empty() && line_has(&v[61], "# c", Tok::Comment));
        let v = view(Lang::Ruby, "x = <<~EOS\n  Don't\n  EOS\nputs 1 # c\nlist << 'item'\n");
        assert!(line_has(&v[1], "  Don't", Tok::Str) && line_has(&v[3], "# c", Tok::Comment) && line_has(&v[4], "'item'", Tok::Str));
        // PowerShell: a backtick escapes outside quotes too
        assert!(line_has(&view(Lang::PowerShell, "Write-Host It`'s\n$x = 1 # c\n")[1], "# c", Tok::Comment));
    }

    #[test]
    fn sql_dialects() {
        // MySQL (here from its dump's first comment): \' in strings and # comments
        let v = view(Lang::Sql, "-- MySQL dump 10.13\nINSERT INTO t VALUES ('It\\'s', \"say \\\"x\\\"\");\n# it's a comment\nSELECT 1;\n");
        assert!(line_has(&v[1], "'It\\'s'", Tok::Str) && line_has(&v[1], "\"say \\\"x\\\"\"", Tok::Str));
        assert_eq!(v[2], vec![("# it's a comment".into(), Tok::Comment)]);
        assert!(line_has(&v[3], "SELECT", Tok::Keyword));
        // a backtick says MySQL too
        assert!(line_has(&view(Lang::Sql, "SELECT `a` FROM t;\nSELECT 'x\\'y' # c\n")[1], "# c", Tok::Comment));
        // otherwise a backslash is just a character (T-SQL paths), and #temp is a table
        let v = view(Lang::Sql, "SELECT 'C:\\' AS p; -- c\nSELECT * INTO #tmp FROM t; -- d\n");
        assert!(line_has(&v[0], "'C:\\'", Tok::Str) && line_has(&v[0], "-- c", Tok::Comment) && line_has(&v[1], "-- d", Tok::Comment));
        assert_eq!(view(Lang::Sql, "# note, it's here\nSELECT 1;\n")[0], vec![("# note, it's here".into(), Tok::Comment)]);
        // PostgreSQL's E'…' strings have escapes
        assert!(line_has(&view(Lang::Sql, "SELECT E'it\\'s' -- c\n")[0], "-- c", Tok::Comment));
    }

    #[test]
    fn php_pages_are_html_with_php_in_them() {
        let src = "<p>Don't panic</p>\n<?php if ($x): // c ?>\n<a href=\"<?= $url ?>\">up</a>\n<?php echo 'x' ?><b>it's</b>\n";
        let v = view(Lang::Php, src);
        assert!(line_has(&v[0], "p", Tok::Tag) && !v[0].iter().any(|t| t.1 == Tok::Str));
        assert!(line_has(&v[1], "<?php", Tok::Control) && line_has(&v[1], "$x", Tok::Var) && line_has(&v[1], "// c ", Tok::Comment));
        assert!(line_has(&v[1], "?>", Tok::Control));
        assert!(line_has(&v[2], "<?=", Tok::Control) && line_has(&v[2], "$url", Tok::Var) && line_has(&v[2], "a", Tok::Tag));
        assert!(line_has(&v[2], "\"", Tok::Str), "the attribute value goes on after ?>: {:?}", v[2]);
        assert!(line_has(&v[3], "'x'", Tok::Str) && line_has(&v[3], "b", Tok::Tag) && !line_has(&v[3], "'s</b>", Tok::Str));
        assert_eq!(end_state(Lang::Php, src), State::START);
        // a pure PHP file
        let v = view(Lang::Php, "<?php\n$s = 'a';\n// done\n");
        assert!(line_has(&v[1], "'a'", Tok::Str) && line_has(&v[2], "// done", Tok::Comment));
        has(Lang::Php, "<?php #[Route('/x')] function f() {}", &[("'/x'", Tok::Str), ("f", Tok::Func)]);
    }

    #[test]
    fn regex_literals() {
        let src = "const p = s.replace(/\\/*$/, '');\nlet x = a / b / c; // d\nif (ok) return /'[/]/g.test(s);\nconst j = <div>x</div>;\n";
        let v = view(Lang::JavaScript, src);
        assert!(line_has(&v[0], "/\\/*$/", Tok::Str) && line_has(&v[0], "''", Tok::Str));
        assert!(line_has(&v[1], "// d", Tok::Comment) && !v[1].iter().any(|t| t.1 == Tok::Str));
        assert!(line_has(&v[2], "/'[/]/g", Tok::Str));
        assert!(!v[3].iter().any(|t| t.1 == Tok::Str));
        assert_eq!(end_state(Lang::JavaScript, src).kind, 0);
        has(Lang::TypeScript, "const r = x.split(/,\\s*/);", &[("/,\\s*/", Tok::Str)]);
    }

    #[test]
    fn csharp_calls_and_c_keywords() {
        has(Lang::CSharp, "Console.WriteLine(\"hi\"); var l = new List<string>(); l.Add(x); var f = new Foo();", &[
            ("Console", Tok::Type),
            ("WriteLine", Tok::Func),
            ("List", Tok::Type),
            ("Add", Tok::Func),
            ("Foo", Tok::Type),
        ]);
        // C isn't C++: `new` and `class` are names there
        assert!(!toks(Lang::C, "struct node *new = class;", State::START).iter().any(|(s, _)| s == "new" || s == "class"));
        has(Lang::Cpp, "auto p = new Foo;", &[("new", Tok::Keyword)]);
        has(Lang::C, "_Bool b; _Static_assert(1, \"x\");", &[("_Bool", Tok::Type), ("_Static_assert", Tok::Keyword)]);
    }

    #[test]
    fn visual_basic_and_autohotkey() {
        let v = view(Lang::Vb, "Option Explicit\nDim s As String: s = \"say \"\"hi\"\"\" ' it's a note\nRem don't\nx = &HFF + #10/8/2026#\nPrivate Sub Main() : Rem it\"s\n#If VBA7 Then\n");
        assert!(line_has(&v[0], "Option", Tok::Keyword) && line_has(&v[1], "String", Tok::Type));
        assert!(line_has(&v[1], "\"say \"", Tok::Str) && line_has(&v[1], "' it's a note", Tok::Comment));
        assert_eq!(v[2], vec![("Rem don't".into(), Tok::Comment)]);
        assert!(line_has(&v[3], "&HFF", Tok::Num) && line_has(&v[3], "#10/8/2026#", Tok::Num));
        assert!(line_has(&v[4], "Main", Tok::Func) && line_has(&v[4], "Rem it\"s", Tok::Comment));
        assert!(line_has(&v[5], "#If", Tok::Control));
        let src = "#Requires AutoHotkey v2.0\n^!s::Send \"x\" ; note\n:*:btw::by the way\nStart:\n/* block\n*/\nx := 'it' ; v2 quotes\nSend a;b\n";
        let v = view(Lang::AutoHotkey, src);
        assert!(line_has(&v[0], "#Requires", Tok::Control) && line_has(&v[1], "^!s::", Tok::Section) && line_has(&v[1], "; note", Tok::Comment));
        assert!(line_has(&v[2], ":*:btw::", Tok::Section) && line_has(&v[2], "by the way", Tok::Str) && line_has(&v[3], "Start:", Tok::Section));
        assert!(line_has(&v[4], "/* block", Tok::Comment) && line_has(&v[5], "*/", Tok::Comment));
        assert!(line_has(&v[6], "'it'", Tok::Str) && !v[7].iter().any(|t| t.1 == Tok::Comment));
        // version 1: commands with a comma, and an apostrophe is just a character
        let v = view(Lang::AutoHotkey, "MsgBox, Don't %name%\n");
        assert!(line_has(&v[0], "MsgBox", Tok::Func) && line_has(&v[0], "%name%", Tok::Var) && !v[0].iter().any(|t| t.1 == Tok::Str));
    }

    #[test]
    fn perl_scripts() {
        let src = "my @a = qw(\n  one two\n);\nprint $#a if $s =~ /a#b/;\n$s =~ s{x}{y}g; # done\nmy $t = <<\"EOF\";\nit's\nEOF\n\n=head1 NAME\n\nit's pod\n\n=cut\nmy $q = $' . $h{x} / 2;\n__END__\nit's data\n";
        let v = view(Lang::Perl, src);
        assert!(line_has(&v[0], "qw", Tok::Keyword) && line_has(&v[1], "  one two", Tok::Str) && line_has(&v[2], ")", Tok::Str));
        assert!(line_has(&v[3], "$#a", Tok::Var) && line_has(&v[3], "/a#b/", Tok::Str) && !v[3].iter().any(|t| t.1 == Tok::Comment));
        assert!(line_has(&v[4], "{x}{y}g", Tok::Str) && line_has(&v[4], "# done", Tok::Comment));
        assert_eq!(v[6], vec![("it's".into(), Tok::Str)]);
        assert!(line_has(&v[9], "=head1 NAME", Tok::Comment) && line_has(&v[11], "it's pod", Tok::Comment) && line_has(&v[13], "=cut", Tok::Comment));
        assert!(line_has(&v[14], "$'", Tok::Var) && !v[14].iter().any(|t| t.1 == Tok::Str), "{:?}", v[14]);
        assert!(line_has(&v[16], "it's data", Tok::Comment));
        // `s => 1` and `-s $file` aren't substitutions
        assert!(!toks(Lang::Perl, "my %h = (s => 1); -s $file;", State::START).iter().any(|t| t.1 == Tok::Str));
    }

    #[test]
    fn r_terraform_and_cmake() {
        has(Lang::R, "df <- data.frame(x = c(1L, NA)) %>% filter(is.na(x)) # note", &[
            ("data.frame", Tok::Func),
            ("1L", Tok::Num),
            ("NA", Tok::Lit),
            ("%>%", Tok::Keyword),
            ("is.na", Tok::Func),
            ("# note", Tok::Comment),
        ]);
        let src = "resource \"aws_instance\" \"web\" {\n  ami = \"${var.ami}-${lookup(var.m, \"k\")}\" # id\n  tags = { Name = \"x\" }\n  policy = <<-EOT\n    it's text\n    EOT\n}\n";
        let v = view(Lang::Hcl, src);
        assert!(line_has(&v[0], "resource", Tok::Keyword) && line_has(&v[0], "\"aws_instance\"", Tok::Str));
        assert!(line_has(&v[1], "ami", Tok::Attr) && line_has(&v[1], "\"${var.ami}-${lookup(var.m, \"k\")}\"", Tok::Str) && line_has(&v[1], "# id", Tok::Comment));
        assert!(line_has(&v[2], "Name", Tok::Attr) && line_has(&v[4], "    it's text", Tok::Str) && line_has(&v[5], "    EOT", Tok::Str));
        assert_eq!(end_state(Lang::Hcl, src), State::START);
        let src = "cmake_minimum_required(VERSION 3.20)\n#[[ a\nbracket comment ]]\nset(SRC [=[\nraw ]] text]=] \"${X}\")\nif(WIN32 AND NOT MSVC) # c\ntarget_link_libraries(app PRIVATE $<TARGET_FILE:lib> $ENV{HOME})\nendif()\n";
        let v = view(Lang::CMake, src);
        assert!(line_has(&v[0], "cmake_minimum_required", Tok::Func) && line_has(&v[0], "VERSION", Tok::Keyword));
        assert!(line_has(&v[2], "bracket comment ]]", Tok::Comment) && line_has(&v[4], "raw ]] text]=]", Tok::Str));
        assert!(line_has(&v[5], "if", Tok::Control) && line_has(&v[5], "AND", Tok::Keyword) && line_has(&v[5], "# c", Tok::Comment));
        assert!(line_has(&v[6], "PRIVATE", Tok::Keyword) && line_has(&v[6], "$<TARGET_FILE:lib>", Tok::Var) && line_has(&v[6], "$ENV{HOME}", Tok::Var));
        assert!(line_has(&v[7], "endif", Tok::Control));
    }

    #[test]
    fn cpp_raw_strings_and_digit_separators_and_csharp_holes() {
        let v = view(Lang::Cpp, "auto n = 1'000'000; char c = 'x';\nauto s = R\"sql(\nSELECT \"it's\" )\" here\n)sql\"; // done\n");
        assert!(line_has(&v[0], "1'000'000", Tok::Num) && line_has(&v[0], "'x'", Tok::Str));
        assert!(line_has(&v[1], "R\"sql(", Tok::Str) && line_has(&v[2], "SELECT \"it's\" )\" here", Tok::Str));
        assert!(line_has(&v[3], ")sql\"", Tok::Str) && line_has(&v[3], "// done", Tok::Comment));
        has(Lang::CSharp, "var s = $\"{(ok ? \"yes\" : \"no\")} {{literal}} {d[\"k\"]}\"; // c", &[
            ("$\"{(ok ? \"yes\" : \"no\")} {{literal}} {d[\"k\"]}\"", Tok::Str),
            ("// c", Tok::Comment),
        ]);
    }

    #[test]
    fn dollar_brace_runs_cost_little() {
        let t = "${".repeat(1 << 20);
        let start = std::time::Instant::now();
        for lang in [Lang::Shell, Lang::PowerShell, Lang::Php, Lang::Ruby] {
            lex(lang, t.as_bytes(), State::START, None);
        }
        assert!(start.elapsed().as_secs_f64() < 2.0, "{:?}", start.elapsed());
    }
}
