//! Syntax coloring for the files people open in a text editor: code, markup, config, data and logs.
//!
//! Each language is a small hand-written lexer that colors one display segment (a line, or a piece of a very long
//! line) and returns the `State` it ends in, so things that span lines — block comments, multi-line strings, XML
//! tags, Markdown code blocks — can be followed: the view keeps the state at checkpoints through the document
//! (`editor::HlIndex`). Huge files skip that and color each line on its own, which is exact for everything that
//! doesn't span lines. Lexers never fail: any bytes are fine, what isn't understood just stays uncolored.

mod code;
mod config;
mod markup;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    Plain,
    Json,
    Log,
    Ini,
    Xml,
    Html,
    Markdown,
    Csv,
    CsvSemi,
    Tsv,
    Yaml,
    Python,
    JavaScript,
    TypeScript,
    C,
    Cpp,
    CSharp,
    Java,
    Kotlin,
    Swift,
    Go,
    Rust,
    Php,
    Ruby,
    Lua,
    Sql,
    PowerShell,
    Batch,
    Shell,
    Dockerfile,
    Css,
    Diff,
    /// VBScript, VBA and Visual Basic.
    Vb,
    AutoHotkey,
    Toml,
    Nginx,
    Apache,
    Perl,
    R,
    /// Terraform and other HCL files.
    Hcl,
    CMake,
    /// Java `.properties` files.
    Properties,
    /// SRT and WebVTT subtitles.
    Subtitles,
    /// iCalendar (`.ics`) and vCard (`.vcf`).
    Calendar,
    /// Visual Studio solutions (`.sln`).
    Sln,
    /// Siemens PPCL, the programs of APOGEE and Desigo field panels.
    Ppcl,
    Dart,
    Scala,
    /// Objective-C (`.m` and `.h` files that read like it).
    ObjC,
    /// Objective-C++ (`.mm`).
    ObjCpp,
    /// G-code for 3D printers and CNC machines.
    GCode,
    /// Inno Setup scripts, with Pascal Script in their `[Code]` section.
    InnoSetup,
    /// NSIS installer scripts.
    Nsis,
}

/// How a language writes comments (for "Toggle comment").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentStyle {
    Line(&'static str),
    /// A line comment that goes after the number each line starts with (PPCL: `00010     C …`).
    AfterNumber(&'static str),
    Block(&'static str, &'static str),
}

impl Lang {
    pub fn label(self) -> &'static str {
        match self {
            Lang::Plain => "Plain text",
            Lang::Json => "JSON",
            Lang::Log => "Log",
            Lang::Ini => "INI / Config",
            Lang::Xml => "XML",
            Lang::Html => "HTML",
            Lang::Markdown => "Markdown",
            Lang::Csv => "CSV",
            Lang::CsvSemi => "CSV (semicolons)",
            Lang::Tsv => "TSV (tabs)",
            Lang::Yaml => "YAML",
            Lang::Python => "Python",
            Lang::JavaScript => "JavaScript",
            Lang::TypeScript => "TypeScript",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::Java => "Java",
            Lang::Kotlin => "Kotlin",
            Lang::Swift => "Swift",
            Lang::Go => "Go",
            Lang::Rust => "Rust",
            Lang::Php => "PHP",
            Lang::Ruby => "Ruby",
            Lang::Lua => "Lua",
            Lang::Sql => "SQL",
            Lang::PowerShell => "PowerShell",
            Lang::Batch => "Batch",
            Lang::Shell => "Shell",
            Lang::Dockerfile => "Dockerfile",
            Lang::Css => "CSS",
            Lang::Diff => "Diff",
            Lang::Vb => "VBScript / VBA",
            Lang::AutoHotkey => "AutoHotkey",
            Lang::Toml => "TOML",
            Lang::Nginx => "nginx",
            Lang::Apache => "Apache config",
            Lang::Perl => "Perl",
            Lang::R => "R",
            Lang::Hcl => "Terraform / HCL",
            Lang::CMake => "CMake",
            Lang::Properties => "Properties",
            Lang::Subtitles => "Subtitles (SRT / VTT)",
            Lang::Calendar => "iCalendar / vCard",
            Lang::Sln => "VS solution",
            Lang::Ppcl => "PPCL",
            Lang::Dart => "Dart",
            Lang::Scala => "Scala",
            Lang::ObjC => "Objective-C",
            Lang::ObjCpp => "Objective-C++",
            Lang::GCode => "G-code",
            Lang::InnoSetup => "Inno Setup",
            Lang::Nsis => "NSIS",
        }
    }

    /// Menu order: plain text, then by name.
    pub const ALL: [Lang; 53] = [
        Lang::Plain,
        Lang::Apache,
        Lang::AutoHotkey,
        Lang::Batch,
        Lang::C,
        Lang::CSharp,
        Lang::Cpp,
        Lang::CMake,
        Lang::Css,
        Lang::Csv,
        Lang::CsvSemi,
        Lang::Dart,
        Lang::Diff,
        Lang::Dockerfile,
        Lang::GCode,
        Lang::Go,
        Lang::Html,
        Lang::Calendar,
        Lang::Ini,
        Lang::InnoSetup,
        Lang::Java,
        Lang::JavaScript,
        Lang::Json,
        Lang::Kotlin,
        Lang::Log,
        Lang::Lua,
        Lang::Markdown,
        Lang::Nginx,
        Lang::Nsis,
        Lang::ObjC,
        Lang::ObjCpp,
        Lang::Perl,
        Lang::Php,
        Lang::PowerShell,
        Lang::Ppcl,
        Lang::Properties,
        Lang::Python,
        Lang::R,
        Lang::Ruby,
        Lang::Rust,
        Lang::Scala,
        Lang::Shell,
        Lang::Sql,
        Lang::Subtitles,
        Lang::Swift,
        Lang::Hcl,
        Lang::Toml,
        Lang::Tsv,
        Lang::TypeScript,
        Lang::Vb,
        Lang::Sln,
        Lang::Xml,
        Lang::Yaml,
    ];

    pub fn comment(self) -> Option<CommentStyle> {
        use CommentStyle::*;
        Some(match self {
            Lang::C
            | Lang::Cpp
            | Lang::CSharp
            | Lang::Java
            | Lang::Kotlin
            | Lang::Swift
            | Lang::Go
            | Lang::Rust
            | Lang::JavaScript
            | Lang::TypeScript
            | Lang::Php
            | Lang::Dart
            | Lang::Scala
            | Lang::ObjC
            | Lang::ObjCpp => Line("//"),
            Lang::Python
            | Lang::Ruby
            | Lang::Shell
            | Lang::PowerShell
            | Lang::Yaml
            | Lang::Dockerfile
            | Lang::Ini
            | Lang::Toml
            | Lang::Nginx
            | Lang::Apache
            | Lang::Perl
            | Lang::R
            | Lang::Hcl
            | Lang::CMake
            | Lang::Properties
            | Lang::Sln => Line("#"),
            Lang::Sql | Lang::Lua => Line("--"),
            Lang::Batch => Line("REM "),
            Lang::Vb => Line("'"),
            // (Inno Setup's `[Code]` section is Pascal, commented with `//`: see `inno_code_line`)
            Lang::AutoHotkey | Lang::GCode | Lang::InnoSetup | Lang::Nsis => Line(";"),
            Lang::Ppcl => AfterNumber("C "),
            Lang::Xml | Lang::Html | Lang::Markdown => Block("<!--", "-->"),
            Lang::Css => Block("/*", "*/"),
            Lang::Plain
            | Lang::Json
            | Lang::Log
            | Lang::Csv
            | Lang::CsvSemi
            | Lang::Tsv
            | Lang::Diff
            | Lang::Subtitles
            | Lang::Calendar => return None,
        })
    }

    /// Whether the path bar and the structure panel are offered for this language.
    pub fn has_structure(self) -> bool {
        matches!(self, Lang::Json | Lang::Xml)
    }

    /// Picks a language from the file's path (or just its name), then from the first bytes.
    pub fn detect(path: Option<&str>, head: &[u8]) -> Lang {
        path.and_then(|p| Lang::from_path(p, head)).unwrap_or_else(|| Lang::sniff(head))
    }

    /// By the file name, and for server configuration also by the folder it's in (`/etc/nginx/sites-available/x`,
    /// `C:\Apache24\conf\extra\httpd-ssl.conf`).
    fn from_path(path: &str, head: &[u8]) -> Option<Lang> {
        let lower = path.to_ascii_lowercase();
        let mut parts = lower.rsplit(['\\', '/']);
        let name = parts.next().unwrap_or("");
        let folders: Vec<&str> = parts.take(6).collect();
        let by_name = Lang::from_name(name, head);
        let server = folders.iter().find_map(|f| {
            // (`nginx-1.25.3`, not `nginx-proxy-manager`)
            if *f == "nginx" || f.strip_prefix("nginx-").is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())) {
                Some(Lang::Nginx)
            } else if matches!(*f, "apache" | "apache2" | "apache22" | "apache24" | "httpd") {
                Some(Lang::Apache)
            } else {
                None
            }
        });
        let Some(server) = server else { return by_name };
        let ext = name.rsplit_once('.').map(|(_, e)| e);
        let site = folders.first().is_some_and(|f| matches!(*f, "sites-available" | "sites-enabled" | "conf.d" | "snippets"));
        // its configuration files (`nginx.conf`, `mime.types`, `fastcgi_params`, sites named after their domain); a
        // page or a log in there stays what it is, a script says so in its `#!` line, and a file without an
        // extension outside the sites' folders (`docs/LICENSE`, a README) only when it reads like nginx's
        let conf = match by_name {
            Some(Lang::Ini) => ext == Some("conf"),
            Some(_) => false,
            None if server == Lang::Nginx => {
                Lang::sniff(head) == Lang::Plain && (site || ((ext.is_none() || ext == Some("types")) && nginx_like(head)))
            }
            None => (site || ext == Some("load")) && Lang::sniff(head) == Lang::Plain,
        };
        if conf { Some(server) } else { by_name }
    }

    fn from_name(lower: &str, head: &[u8]) -> Option<Lang> {
        match lower {
            "dockerfile" | "containerfile" => return Some(Lang::Dockerfile),
            "makefile" | "gnumakefile" | ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile"
            | ".gitignore" | ".gitattributes" | ".dockerignore" | ".npmignore" => return Some(Lang::Shell),
            "gemfile" | "rakefile" | "podfile" | "vagrantfile" => return Some(Lang::Ruby),
            "cmakelists.txt" => return Some(Lang::CMake),
            // Groovy, which the Java colors suit
            "jenkinsfile" => return Some(Lang::Java),
            "hosts" => return Some(Lang::Ini),
            "cargo.lock" | "pipfile" | "poetry.lock" | "uv.lock" | "pdm.lock" => return Some(Lang::Toml),
            "nginx.conf" => return Some(Lang::Nginx),
            "httpd.conf" | "apache2.conf" | "apache.conf" => return Some(Lang::Apache),
            _ => {}
        }
        if lower.starts_with("dockerfile.") {
            return Some(Lang::Dockerfile);
        }
        // .env.local, .env.production
        if lower.starts_with(".env.") {
            return Some(Lang::Ini);
        }
        let (stem, ext) = lower.rsplit_once('.')?;
        Some(match ext {
            // templates: colored as what they make (`values.yaml.j2`)
            "j2" | "jinja" | "jinja2" => return Lang::from_name(stem, head),
            "json" | "jsonl" | "ndjson" | "geojson" | "jsonc" | "json5" | "har" | "webmanifest" | "ipynb" | "tfstate" => {
                Lang::Json
            }
            "log" | "out" => Lang::Log,
            "conf" => conf_kind(head),
            "ini" | "cfg" | "env" | "inf" | "reg" | "desktop" | "editorconfig" | "gitconfig" | "gitmodules" | "npmrc"
            | "pypirc" | "cnf" | "wslconfig" | "service" | "socket" | "timer" | "mount" => Lang::Ini,
            "toml" => Lang::Toml,
            "properties" => Lang::Properties,
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "xaml" | "csproj" | "vbproj" | "fsproj" | "vcxproj" | "proj"
            | "props" | "targets" | "nuspec" | "resx" | "config" | "plist" | "kml" | "gpx" | "rss" | "atom" | "wsdl"
            | "xlf" | "xliff" | "manifest" | "ps1xml" | "storyboard" | "xib" | "fxml" | "dtd" | "slnx" => Lang::Xml,
            "html" | "htm" | "xhtml" | "shtml" | "vue" | "svelte" | "cshtml" | "razor" | "jsp" | "asp" | "aspx" => {
                Lang::Html
            }
            "md" | "markdown" | "mdown" | "mkd" | "mdx" => Lang::Markdown,
            "csv" => csv_kind(head),
            "tsv" | "tab" => Lang::Tsv,
            "yaml" | "yml" => Lang::Yaml,
            "py" | "pyw" | "pyi" | "pyx" => Lang::Python,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Lang::TypeScript,
            "c" => Lang::C,
            "h" if objc_like(head) => Lang::ObjC,
            "h" if cpp_like(head) => Lang::Cpp,
            "h" => Lang::C,
            // Objective-C, or MATLAB (left to what the content says)
            "m" => return objc_like(head).then_some(Lang::ObjC),
            // (FreeMind and Freeplane mind maps are `.mm` files too: XML, which the content tells)
            "mm" => return (!head.trim_ascii_start().starts_with(b"<")).then_some(Lang::ObjCpp),
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ino" | "inl" | "ipp" | "tpp" => Lang::Cpp,
            "cs" | "csx" => Lang::CSharp,
            "java" | "gradle" | "groovy" => Lang::Java,
            "kt" | "kts" => Lang::Kotlin,
            "swift" => Lang::Swift,
            "go" => Lang::Go,
            "rs" => Lang::Rust,
            "php" | "phtml" | "php3" | "php4" | "php5" | "phps" => Lang::Php,
            "rb" | "rake" | "gemspec" | "ru" => Lang::Ruby,
            "lua" => Lang::Lua,
            "sql" | "ddl" | "dml" | "psql" | "pgsql" | "mysql" => Lang::Sql,
            "ps1" | "psm1" | "psd1" => Lang::PowerShell,
            "bat" | "cmd" | "btm" => Lang::Batch,
            "sh" | "bash" | "zsh" | "ksh" | "fish" | "mk" => Lang::Shell,
            "dockerfile" => Lang::Dockerfile,
            "css" | "scss" | "sass" | "less" => Lang::Css,
            "diff" | "patch" | "rej" => Lang::Diff,
            "vbs" | "vba" | "bas" | "vb" => Lang::Vb,
            // a VBA class module, or a LaTeX class (left plain)
            "cls" => return (!matches!(head.trim_ascii_start().first(), Some(b'%' | b'\\'))).then_some(Lang::Vb),
            "ahk" | "ah2" => Lang::AutoHotkey,
            "pl" | "pm" | "pod" | "psgi" => Lang::Perl,
            "r" | "rprofile" => Lang::R,
            "tf" | "tfvars" | "hcl" | "nomad" => Lang::Hcl,
            "cmake" => Lang::CMake,
            "srt" | "vtt" => Lang::Subtitles,
            "ics" | "ical" | "ifb" | "vcs" | "vcf" | "vcard" => Lang::Calendar,
            "sln" => Lang::Sln,
            // (a `.pcl` file is PPCL only when it reads like it: it's also HP's printer language)
            "ppcl" => Lang::Ppcl,
            "htaccess" => Lang::Apache,
            "dart" => Lang::Dart,
            "scala" | "sc" | "sbt" => Lang::Scala,
            "gcode" | "gco" | "ngc" | "cnc" => Lang::GCode,
            // G-code, or NetCDF data (binary: "CDF\x01", or HDF5's header, with zero bytes)
            "nc" => return (!head.starts_with(b"CDF") && !head.contains(&0)).then_some(Lang::GCode),
            "iss" | "isl" => Lang::InnoSetup,
            "nsi" | "nsh" => Lang::Nsis,
            // rotated logs: app.log.1, app.log.2026-10-07
            _ if lower.contains(".log.") => Lang::Log,
            _ => return None,
        })
    }

    /// Recognizes a file by its first bytes (a `.txt` holding JSON, a script with a `#!` line...).
    fn sniff(head: &[u8]) -> Lang {
        let t = head.trim_ascii_start();
        if t.starts_with(b"#!") {
            let line = t.split(|&b| b == b'\n').next().unwrap_or(b"").to_ascii_lowercase();
            let has = |w: &[u8]| line.windows(w.len()).any(|x| x == w);
            return if has(b"python") {
                Lang::Python
            } else if has(b"node") || has(b"deno") || has(b"bun") {
                Lang::JavaScript
            } else if has(b"pwsh") || has(b"powershell") {
                Lang::PowerShell
            } else if has(b"ruby") {
                Lang::Ruby
            } else if has(b"lua") {
                Lang::Lua
            } else if has(b"php") {
                Lang::Php
            } else if has(b"perl") {
                Lang::Perl
            } else if has(b"rscript") {
                Lang::R
            } else {
                Lang::Shell
            };
        }
        let starts_ci = |p: &[u8]| t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p);
        if starts_ci(b"<?xml") {
            return Lang::Xml;
        }
        if starts_ci(b"<?php") {
            return Lang::Php;
        }
        if starts_ci(b"<!doctype html") || starts_ci(b"<html") {
            return Lang::Html;
        }
        if starts_ci(b"webvtt") || is_srt(t) {
            return Lang::Subtitles;
        }
        if starts_ci(b"begin:vcalendar") || starts_ci(b"begin:vcard") {
            return Lang::Calendar;
        }
        if t.starts_with(b"Microsoft Visual Studio Solution File") {
            return Lang::Sln;
        }
        // `<VirtualHost *:80>` isn't XML
        if let Some(l) = server_conf(t, 1) {
            return l;
        }
        if t.starts_with(b"<!--") || (t.first() == Some(&b'<') && t.get(1).is_some_and(|c| c.is_ascii_alphabetic())) {
            return Lang::Xml;
        }
        if t.starts_with(b"diff --git ") || (t.starts_with(b"--- ") && t.windows(5).any(|w| w == b"\n+++ ")) {
            return Lang::Diff;
        }
        if matches!(t.first(), Some(b'{') | Some(b'[')) {
            // "[section]" lines are INI, not JSON arrays.
            if t.first() == Some(&b'[') {
                let line = t.split(|&b| b == b'\n').next().unwrap_or(b"").trim_ascii_end();
                if line.ends_with(b"]")
                    && line.len() > 2
                    && line[1].is_ascii_alphabetic()
                    && line.iter().all(|&b| b.is_ascii_alphanumeric() || b" []._-:".contains(&b))
                {
                    return Lang::Ini;
                }
            }
            // `[2026-10-07 12:00:01] INFO …` is a log and `[ ] buy milk` a list: only what reads as JSON is JSON
            if !looks_like_log(head) && looks_like_json(t) {
                return Lang::Json;
            }
        }
        if looks_like_ppcl(t) {
            return Lang::Ppcl;
        }
        if looks_like_log(head) {
            return Lang::Log;
        }
        if looks_like_gcode(head) {
            return Lang::GCode;
        }
        Lang::Plain
    }
}

/// A PPCL program as Desigo and APOGEE export it: every line starts with its number (`00010     C comment`,
/// `20    IF(…) THEN …`, `# ` before it on a line turned off), at least half of them are comments, commands (right
/// before their `(`), jumps or assignments, and one is a command or an assignment. When the numbers aren't the five
/// digits exports write, something only PPCL has must be there too: a dotted operator (`.EQ.`), a command on a point
/// (`ON("FAN")`) or a point given a value (`"FAN" = 1`). A numbered list or a BASIC program doesn't read like that.
fn looks_like_ppcl(t: &[u8]) -> bool {
    let (mut lines, mut ppcl, mut commands, mut comments) = (0, 0, 0, 0);
    let (mut padded, mut sure) = (true, false);
    let mut rest = t.split(|&b| b == b'\n').peekable();
    while let Some(l) = rest.next() {
        let l = l.trim_ascii_end();
        if l.is_empty() {
            continue;
        }
        // (the sample's last line can be cut short, its number too)
        let cut = rest.peek().is_none() && !t.ends_with(b"\n");
        let l = l.strip_prefix(b"#").map_or(l, |r| r.trim_ascii_start());
        let d = l.iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=5).contains(&d) || !matches!(l.get(d), None | Some(b' ' | b'\t')) {
            if cut {
                break;
            }
            return false;
        }
        padded &= d == 5 || cut;
        let s = l[d..].trim_ascii_start();
        let (word, after) = s.split_at(s.iter().take_while(|b| b.is_ascii_alphanumeric()).count());
        let comment = word.eq_ignore_ascii_case(b"C") && matches!(after.first(), None | Some(b' ' | b'\t'));
        let command =
            after.starts_with(b"(") && (word.eq_ignore_ascii_case(b"IF") || config::ppcl_in(&config::PPCL_COMMANDS, word));
        let jump = [&b"GOTO"[..], b"GOSUB", b"RETURN"].iter().any(|j| j.eq_ignore_ascii_case(word));
        // `"POINT" = …`, `$LOC1 = …`
        let name = match s.first() {
            Some(b'"') => s[1..].iter().position(|&b| b == b'"').map_or(0, |p| p + 2),
            Some(b'$') => 1 + s[1..].iter().take_while(|b| b.is_ascii_alphanumeric()).count(),
            _ => 0,
        };
        let assignment = name > 0 && s[name..].trim_ascii_start().starts_with(b"=");
        lines += 1;
        ppcl += (comment || command || jump || assignment) as usize;
        commands += (command || assignment) as usize;
        comments += comment as usize;
        if !comment {
            sure |= (command && after.starts_with(b"(\""))
                || (assignment && s[0] == b'"')
                || config::ppcl_has_operator(s);
        }
        if lines == 64 {
            break;
        }
    }
    // (or a long header of comments, all the sample has)
    lines >= 2 && (padded || sure) && ((commands > 0 && ppcl * 2 >= lines) || (padded && comments == lines))
}

/// Whether a `.m` or `.h` file reads like Objective-C: a line starts with `#import`, `@import`, `@interface`,
/// `@implementation`, `@protocol` or `@class`.
fn objc_like(head: &[u8]) -> bool {
    const WORDS: [&[u8]; 5] = [b"@import", b"@interface", b"@implementation", b"@protocol", b"@class"];
    // (lines inside a `/* */` comment don't count: Doxygen writes `@class` there)
    let mut comment = false;
    for l in head.split(|&b| b == b'\n').map(|l| l.trim_ascii()) {
        if comment {
            comment = memchr::memmem::find(l, b"*/").is_none();
            continue;
        }
        if let Some(rest) = l.strip_prefix(b"#import") {
            // `#import <Foundation/Foundation.h>`, `#import "View.h"`; not a type library (`#import "msxml6.dll"`,
            // Visual C++) nor Octave's `#import data`
            let rest = rest.trim_ascii_start();
            let close = match rest.first() {
                Some(b'<') => b'>',
                Some(b'"') => b'"',
                _ => 0,
            };
            if close != 0 {
                let name = &rest[1..rest[1..].iter().position(|&b| b == close).map_or(rest.len(), |p| p + 1)];
                let name = name.to_ascii_lowercase();
                let library = [&b".dll"[..], b".tlb", b".olb", b".ocx", b".exe"].iter().any(|e| name.ends_with(e))
                    || name.starts_with(b"progid:")
                    || name.starts_with(b"libid:");
                if !library {
                    return true;
                }
            }
        } else if WORDS.iter().any(|w| l.starts_with(w) && matches!(l.get(w.len()), Some(b' ' | b'\t' | b'<' | b';') | None)) {
            return true;
        }
        if let Some(p) = memchr::memmem::find(l, b"/*") {
            comment = memchr::memmem::find(&l[p + 2..], b"*/").is_none();
        }
    }
    false
}

/// Whether a `.h` file reads like C++: a line starts with `class Name`, `namespace`, `template <` or `using
/// namespace`, is a `public:`-style label, uses `std::`, or includes a standard header without `.h` (`<vector>`).
/// (Comments don't count.)
fn cpp_like(head: &[u8]) -> bool {
    let mut comment = false;
    for l in head.split(|&b| b == b'\n').map(|l| l.trim_ascii()) {
        if comment {
            comment = memchr::memmem::find(l, b"*/").is_none();
            continue;
        }
        let code = &l[..memchr::memmem::find(l, b"//").unwrap_or(l.len())];
        let word = |w: &[u8]| code.starts_with(w) && matches!(code.get(w.len()), Some(b' ' | b'\t' | b'<') | None);
        if word(b"class")
            || word(b"namespace")
            || word(b"template")
            || code.starts_with(b"using namespace ")
            || matches!(code.trim_ascii_end(), b"public:" | b"private:" | b"protected:")
            || memchr::memmem::find(code, b"std::").is_some()
        {
            return true;
        }
        if let Some(rest) = code.strip_prefix(b"#include").map(|r| r.trim_ascii_start()) {
            let name = rest.strip_prefix(b"<").and_then(|r| r.split(|&b| b == b'>').next()).unwrap_or(b"");
            if !name.is_empty() && name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'/') {
                return true;
            }
        }
        if let Some(p) = memchr::memmem::find(code, b"/*") {
            comment = memchr::memmem::find(&code[p + 2..], b"*/").is_none();
        }
    }
    false
}

/// Whether text reads like G-code: a slicer's header comment (`;FLAVOR:Marlin`, `; generated by PrusaSlicer …`), or
/// mostly lines of words like `G1 X10.5 F1200` (at least three, the comments aside).
fn looks_like_gcode(head: &[u8]) -> bool {
    const SLICERS: [&[u8]; 11] = [
        b"prusaslicer", b"superslicer", b"orcaslicer", b"bambustudio", b"slic3r", b"cura_steamengine", b"simplify3d",
        b"kisslicer", b"ideamaker", b"craftware", b"flashprint",
    ];
    let (mut code, mut other, mut g) = (0, 0, false);
    for l in head.split(|&b| b == b'\n').map(|l| l.trim_ascii()).filter(|l| !l.is_empty()).take(40) {
        if l[0] == b';' {
            let low = l.to_ascii_lowercase();
            if low.starts_with(b";flavor:") || SLICERS.iter().any(|s| low.windows(s.len()).any(|w| w == *s)) {
                return true;
            }
        } else if l[0] != b'(' && l != b"%" {
            if gcode_words(l) {
                code += 1;
                g |= l.iter().enumerate().any(|(i, &c)| {
                    matches!(c, b'G' | b'g') && at(l, i + 1).is_ascii_digit() && (i == 0 || !l[i - 1].is_ascii_alphabetic())
                });
            } else {
                other += 1;
            }
            if code + other >= 12 {
                break;
            }
        }
    }
    // (with a `G` word: `M3x10`, `T100` or `N40.7128 W74.0060` lists aren't G-code)
    code >= 3 && other * 5 <= code && g
}

/// Whether a line (up to a comment) is G-code words only: a letter and a number each (`N10 G1 X-1.5 Y.5`,
/// `G1X10Y20`), the first one a `G`, `M`, `N`, `T` or `O`.
fn gcode_words(l: &[u8]) -> bool {
    let l = &l[..memchr::memchr2(b';', b'(', l).unwrap_or(l.len())];
    let (mut i, mut words) = (0, 0);
    while i < l.len() {
        if matches!(l[i], b' ' | b'\t' | b'\r') {
            i += 1;
            continue;
        }
        let c = l[i].to_ascii_uppercase();
        if !c.is_ascii_uppercase() || (words == 0 && !b"GMNTO".contains(&c)) {
            return false;
        }
        i += 1 + matches!(at(l, i + 1), b'+' | b'-') as usize;
        let d = l[i..].iter().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
        if !l[i..i + d].iter().any(u8::is_ascii_digit) {
            return false;
        }
        i += d;
        words += 1;
    }
    words > 0
}

/// A `.conf` file: XML, nginx's or Apache's configuration, or else INI-like.
fn conf_kind(head: &[u8]) -> Lang {
    if head.trim_ascii_start().starts_with(b"<?xml") {
        return Lang::Xml;
    }
    server_conf(head, 200).unwrap_or(Lang::Ini)
}

/// nginx or Apache configuration, as told by the first `lines` lines that aren't comments or blank: an nginx block
/// (`server {`, `location / {`) or directive (`worker_processes 4;`), an Apache section (`<VirtualHost *:80>`) or
/// directive (`LoadModule …`, `DocumentRoot …`).
fn server_conf(head: &[u8], lines: usize) -> Option<Lang> {
    const NGINX_BLOCKS: [&[u8]; 6] = [b"events", b"http", b"location", b"server", b"stream", b"upstream"];
    const NGINX_DIRECTIVES: [&[u8]; 12] = [
        b"access_log", b"error_log", b"keepalive_timeout", b"listen", b"proxy_pass", b"proxy_set_header", b"root",
        b"sendfile", b"server_name", b"user", b"worker_connections", b"worker_processes",
    ];
    const APACHE_SECTIONS: [&[u8]; 12] = [
        b"directory", b"directorymatch", b"files", b"filesmatch", b"ifdefine", b"ifmodule", b"ifversion", b"location",
        b"locationmatch", b"proxy", b"virtualhost", b"macro",
    ];
    const APACHE_DIRECTIVES: [&[u8]; 14] = [
        b"AddHandler", b"AddType", b"AllowOverride", b"CustomLog", b"DirectoryIndex", b"DocumentRoot", b"ErrorDocument",
        b"ErrorLog", b"LoadModule", b"RewriteCond", b"RewriteEngine", b"RewriteRule", b"ServerName", b"ServerRoot",
    ];
    let word_end = |l: &[u8]| l.iter().position(|&b| !(b.is_ascii_alphanumeric() || b == b'_')).unwrap_or(l.len());
    for l in head.split(|&b| b == b'\n').map(|l| l.trim_ascii()).filter(|l| !l.is_empty() && l[0] != b'#').take(lines) {
        let w = &l[..word_end(l)];
        let spaced = matches!(l.get(w.len()), Some(b' ' | b'\t'));
        if let Some(sec) = l.strip_prefix(b"<") {
            // (an XML element has attributes with `=`: `<location lat="1">`)
            let w = &sec[..word_end(sec)];
            let args = &sec[w.len()..sec.iter().position(|&b| b == b'>').unwrap_or(sec.len())];
            if APACHE_SECTIONS.iter().any(|s| s.eq_ignore_ascii_case(w))
                && matches!(args.first(), Some(b' ' | b'\t'))
                && !args.contains(&b'=')
            {
                return Some(Lang::Apache);
            }
            continue;
        }
        let rest = l[w.len()..].trim_ascii_start();
        if NGINX_BLOCKS.contains(&w) && (rest.starts_with(b"{") || (spaced && l.ends_with(b"{"))) {
            return Some(Lang::Nginx);
        }
        if NGINX_DIRECTIVES.contains(&w) && spaced && l.ends_with(b";") {
            return Some(Lang::Nginx);
        }
        if APACHE_DIRECTIVES.contains(&w) && spaced && !l.ends_with(b";") {
            return Some(Lang::Apache);
        }
    }
    None
}

/// Whether text reads like nginx's configuration: its first lines that aren't comments start with a directive's
/// name (or close a block), and most end like directives or blocks do (`;`, `{`, `}`) — not a README's sentences.
fn nginx_like(head: &[u8]) -> bool {
    let lines: Vec<&[u8]> = head
        .split(|&b| b == b'\n')
        .map(|l| {
            // without a comment at its end
            let l = l.trim_ascii();
            let code = l.windows(2).position(|w| w[0].is_ascii_whitespace() && w[1] == b'#').map_or(l, |p| &l[..p]);
            code.trim_ascii_end()
        })
        .filter(|l| !l.is_empty() && l[0] != b'#')
        .take(8)
        .collect();
    let starts = lines.first().is_some_and(|l| l[0].is_ascii_lowercase() || l[0] == b'_' || l[0] == b'}');
    let ends = lines.iter().filter(|l| matches!(l.last(), Some(b';' | b'{' | b'}'))).count();
    starts && ends * 2 >= lines.len()
}

/// An SRT file: a cue number, then a `00:00:01,000 --> 00:00:04,000` line.
fn is_srt(t: &[u8]) -> bool {
    let mut lines = t.split(|&b| b == b'\n').map(|l| l.trim_ascii());
    let num = lines.next().is_some_and(|l| !l.is_empty() && l.len() < 10 && l.iter().all(u8::is_ascii_digit));
    num && lines.next().is_some_and(|l| l.len() < 80 && l.windows(3).any(|w| w == b"-->") && l.first().is_some_and(u8::is_ascii_digit))
}

/// Whether the start of a file reads as JSON (or JSON with comments) as far as it goes.
fn looks_like_json(head: &[u8]) -> bool {
    use crate::core::json;
    match json::Formatter::new(json::Mode::Validate, b"", b"\n", None, None).feed(head) {
        Ok(()) => true,
        Err(e) => e.msg == json::COMMENTS,
    }
}

/// Which separator a CSV file uses: the one its first line has most of outside quotes (tabs, semicolons or commas).
fn csv_kind(head: &[u8]) -> Lang {
    let (mut quoted, mut n) = (false, [0usize; 3]);
    for &b in head.split(|&b| b == b'\n').next().unwrap_or(b"") {
        match b {
            b'"' => quoted = !quoted,
            b',' if !quoted => n[0] += 1,
            b';' if !quoted => n[1] += 1,
            b'\t' if !quoted => n[2] += 1,
            _ => {}
        }
    }
    if n[2] > n[0] && n[2] > n[1] {
        Lang::Tsv
    } else if n[1] > n[0] {
        Lang::CsvSemi
    } else {
        Lang::Csv
    }
}

/// Whether most of the first lines start with a date or time, like log files do. (An IP address isn't a time: a
/// hosts file isn't a log, but a web server's access log, `10.0.0.1 - - [07/Oct/2026:12:00:01 …`, is.)
fn looks_like_log(head: &[u8]) -> bool {
    let lines: Vec<&[u8]> = head.split(|&b| b == b'\n').filter(|l| !l.trim_ascii().is_empty()).take(5).collect();
    let stamped = lines
        .iter()
        .filter(|l| {
            if let Some(e) = ipv4_end(l) {
                return l[e..].iter().take(48).zip(l[e..].iter().skip(1)).any(|(&a, b)| a == b'[' && b.is_ascii_digit());
            }
            let l = l.strip_prefix(b"[").unwrap_or(l);
            let p = &l[..l.len().min(12)];
            l.first().is_some_and(|c| c.is_ascii_digit())
                && p.iter().filter(|c| c.is_ascii_digit()).count() >= 6
                && p.iter().filter(|c| matches!(c, b'-' | b'/' | b':' | b'.')).count() >= 2
        })
        .count();
    !lines.is_empty() && stamped * 2 > lines.len()
}

/// Where an IPv4 address that starts `l` ends (it is followed by a space or a tab).
fn ipv4_end(l: &[u8]) -> Option<usize> {
    let mut i = 0;
    for part in 0..4 {
        let d = l[i..].iter().take(4).take_while(|b| b.is_ascii_digit()).count();
        if !(1..=3).contains(&d) {
            return None;
        }
        i += d;
        if part < 3 {
            if l.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    matches!(l.get(i), Some(b' ' | b'\t')).then_some(i)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Key,
    Str,
    Num,
    Lit,
    Punct,
    Comment,
    Section,
    Error,
    Warn,
    Info,
    Dim,
    Keyword,
    Control,
    Type,
    Func,
    Tag,
    Attr,
    Var,
    Heading,
    Bold,
    Italic,
    Link,
    Added,
    Removed,
    /// A CSV column (1..7; column 0 and every 8th keep the normal text color).
    Col(u8),
}

/// A colored byte range of a segment.
pub type Span = (u32, u32, Tok);

/// Where a lexer is at some byte position: inside nothing, a block comment, a string... Small and comparable, so
/// colored layouts can be cached by the state they start in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct State {
    /// What we're inside (each lexer has its own meanings; 0 = nothing special).
    kind: u8,
    /// A detail of `kind`: the quote character, a nesting depth, a fence character...
    a: u8,
    /// A second detail: a CSV column, a YAML block indent, a fence length, a CSS brace depth...
    b: u16,
    /// HTML: 1 inside `<script>`, 2 inside `<style>` (`kind`, `a`, `b` then belong to that language).
    mode: u8,
    /// At the very start of a line.
    col0: bool,
    /// Nothing but whitespace so far on this line.
    bol: bool,
}

impl State {
    /// The start of a document.
    pub const START: State = State { kind: 0, a: 0, b: 0, mode: 0, col0: true, bol: true };
    /// Somewhere in the middle of a line, inside nothing.
    pub const MID_LINE: State = State { kind: 0, a: 0, b: 0, mode: 0, col0: false, bol: false };
}

/// Where spans go (nowhere when only the end state is wanted), and the offset added to them.
struct Out<'a> {
    v: Option<&'a mut Vec<Span>>,
    base: usize,
}

impl Out<'_> {
    #[inline]
    fn put(&mut self, s: usize, e: usize, tok: Tok) {
        if e > s {
            if let Some(v) = self.v.as_mut() {
                v.push(((s + self.base) as u32, (e + self.base) as u32, tok));
            }
        }
    }

    fn on(&self) -> bool {
        self.v.is_some()
    }

    /// The same output, for a slice starting `off` bytes later.
    fn at(&mut self, off: usize) -> Out<'_> {
        Out { v: self.v.as_deref_mut(), base: self.base + off }
    }
}

/// Colors `text` starting in state `st` and returns the state after it. Spans (byte ranges within `text`) go to
/// `out` when given; without it only the state is worked out (cheaper).
pub fn lex(lang: Lang, text: &[u8], st: State, out: Option<&mut Vec<Span>>) -> State {
    let mut o = Out { v: out, base: 0 };
    if let Some(v) = o.v.as_mut() {
        v.clear();
    }
    let mut end = lex_in(lang, text, st, &mut o);
    line_flags(text, st, &mut end);
    end
}

/// `lex` without the line flags, adding to what `o` has.
fn lex_in(lang: Lang, text: &[u8], st: State, o: &mut Out) -> State {
    match lang {
        Lang::Plain => st,
        Lang::Json => json(text, st, o),
        Lang::Log => by_line(text, st, o, log_line),
        Lang::Ini => by_line(text, st, o, ini_line),
        Lang::Diff => diff(text, st, o),
        Lang::Csv => csv(text, st, b',', o),
        Lang::CsvSemi => csv(text, st, b';', o),
        Lang::Tsv => csv(text, st, b'\t', o),
        Lang::Xml | Lang::Html => markup::markup(lang == Lang::Html, text, st, o),
        Lang::Php => markup::php(text, st, o),
        Lang::Markdown => markup::markdown(text, st, o),
        Lang::Yaml => markup::yaml(text, st, o),
        Lang::Css => code::css(text, st, o),
        Lang::Toml => config::toml(text, st, o),
        Lang::Nginx => config::nginx(text, st, o),
        Lang::Apache => config::apache(text, st, o),
        Lang::Properties => config::properties(text, st, o),
        Lang::Subtitles => config::subtitles(text, st, o),
        Lang::Calendar => by_line(text, st, o, config::calendar_line),
        Lang::Sln => by_line(text, st, o, config::sln_line),
        Lang::Ppcl => config::ppcl(text, st, o),
        Lang::GCode => by_line(text, st, o, config::gcode_line),
        Lang::InnoSetup => config::inno(text, st, o),
        _ => code::code(code::syntax(lang), text, st, o),
    }
}

/// Whether a line of an Inno Setup script, after `before` (the text before it), is in its `[Code]` section — Pascal,
/// commented with `//` — and isn't a section's header itself (for "Toggle comment").
pub fn inno_code_line(before: &[u8], line: &[u8]) -> bool {
    let l = line.trim_ascii();
    let header = l.len() > 2 && l[0] == b'[' && l[l.len() - 1] == b']' && l[1..l.len() - 1].iter().all(u8::is_ascii_alphabetic);
    !header && lex(Lang::InnoSetup, before, State::START, None).kind & config::I_CODE != 0
}

/// The state a segment of a huge file starts in when it doesn't start its line: worked out from up to a few KB
/// of that line before it (`from_line_start`: whether `prefix` starts the line).
pub fn guess(lang: Lang, prefix: &[u8], from_line_start: bool) -> State {
    if lang == Lang::Json && !from_line_start {
        return State { kind: if json_starts_in_string(prefix) { J_STR } else { 0 }, ..State::MID_LINE };
    }
    lex(lang, prefix, if from_line_start { State::START } else { State::MID_LINE }, None)
}

fn line_flags(t: &[u8], st: State, end: &mut State) {
    match memchr::memrchr(b'\n', t) {
        Some(p) => {
            end.col0 = p + 1 == t.len();
            end.bol = t[p + 1..].iter().all(|b| b.is_ascii_whitespace());
        }
        None => {
            end.col0 = st.col0 && t.is_empty();
            end.bol = st.bol && t.iter().all(|b| b.is_ascii_whitespace());
        }
    }
}

// ---- helpers shared by the lexers ----

/// The byte at `i`, or 0 past the end.
#[inline]
fn at(t: &[u8], i: usize) -> u8 {
    t.get(i).copied().unwrap_or(0)
}

/// End of the line containing `i` (the position of its `\n`, or the end of `t`).
fn line_end(t: &[u8], i: usize) -> usize {
    memchr::memchr(b'\n', &t[i.min(t.len())..]).map_or(t.len(), |p| i + p)
}

fn find(t: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    memchr::memmem::find(t.get(from..)?, pat).map(|p| from + p)
}

/// Scans a string body from `i` up to its closing `quote`. `esc` is the escape character (0: none), `esc_pending`
/// whether the text starts right after one. A one-line string ends at the line break even when unclosed. Returns
/// (end, closed, escape pending at the end).
fn scan_str(t: &[u8], mut i: usize, quote: u8, esc: u8, mut esc_pending: bool, one_line: bool) -> (usize, bool, bool) {
    while i < t.len() {
        let b = t[i];
        if esc_pending {
            esc_pending = false;
        } else if b == esc && esc != 0 {
            esc_pending = true;
        } else if b == quote {
            return (i + 1, true, false);
        } else if b == b'\n' && one_line {
            return (i, true, false);
        }
        i += 1;
    }
    (i, false, esc_pending)
}

fn is_num_word(w: &[u8]) -> bool {
    let w = w.strip_prefix(b"-").or_else(|| w.strip_prefix(b"+")).unwrap_or(w);
    !w.is_empty()
        && w[0].is_ascii_digit()
        && w.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b':'))
}

// ---- JSON ----

const J_STR: u8 = 1;
const J_LINE_COMMENT: u8 = 2;
const J_BLOCK_COMMENT: u8 = 3;

/// Whether a JSON segment that doesn't start its line starts inside a string, judged from the bytes before it:
/// exact from the line start, otherwise a guess from the last quote that clearly closed a string.
fn json_starts_in_string(prefix: &[u8]) -> bool {
    // A quote followed by ':' ',' '}' or ']' almost always ends a string, unless it is escaped (`\"` in a string
    // holding JSON itself: `"payload": "{\"a\":1}"`).
    let escaped = |q: usize| prefix[..q].iter().rev().take_while(|&&b| b == b'\\').count() % 2 == 1;
    let Some(start) = (1..prefix.len())
        .rev()
        .find(|&i| matches!(prefix[i], b':' | b',' | b'}' | b']') && prefix[i - 1] == b'"' && !escaped(i - 1))
    else {
        return false;
    };
    let mut in_str = false;
    let mut esc = false;
    for &b in &prefix[start..] {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        }
    }
    in_str
}

fn json(t: &[u8], st: State, o: &mut Out) -> State {
    let n = t.len();
    let mut i = 0;
    // Strings are keys when a ':' follows.
    let string = |o: &mut Out, s: usize, e: usize| {
        let j = e + t[e..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        o.put(s, e, if at(t, j) == b':' { Tok::Key } else { Tok::Str });
    };
    match st.kind {
        J_STR => {
            let (e, closed, esc) = scan_str(t, 0, b'"', b'\\', st.a == 1, true);
            string(o, 0, e);
            if !closed {
                return State { kind: J_STR, a: esc as u8, ..st };
            }
            i = e;
        }
        J_LINE_COMMENT => {
            i = line_end(t, 0);
            o.put(0, i, Tok::Comment);
            if i == n {
                return st;
            }
        }
        J_BLOCK_COMMENT => match find(t, 0, b"*/") {
            Some(p) => {
                o.put(0, p + 2, Tok::Comment);
                i = p + 2;
            }
            None => {
                o.put(0, n, Tok::Comment);
                return st;
            }
        },
        _ => {}
    }
    while i < n {
        let b = t[i];
        match b {
            b'"' => {
                let (e, closed, esc) = scan_str(t, i + 1, b'"', b'\\', false, true);
                string(o, i, e);
                if !closed {
                    return State { kind: J_STR, a: esc as u8, b: 0, ..st };
                }
                i = e;
            }
            b'-' | b'0'..=b'9' => {
                let s = i;
                while i < n && matches!(t[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                o.put(s, i, Tok::Num);
            }
            b't' | b'f' | b'n' => {
                let s = i;
                while i < n && t[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if matches!(&t[s..i], b"true" | b"false" | b"null") {
                    o.put(s, i, Tok::Lit);
                }
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                let s = i;
                while i < n && matches!(t[i], b'{' | b'}' | b'[' | b']' | b':' | b',') {
                    i += 1;
                }
                o.put(s, i, Tok::Punct);
            }
            // JSONC / JSON5 comments
            b'/' if at(t, i + 1) == b'/' => {
                let e = line_end(t, i);
                o.put(i, e, Tok::Comment);
                if e == n {
                    return State { kind: J_LINE_COMMENT, a: 0, b: 0, ..st };
                }
                i = e;
            }
            b'/' if at(t, i + 1) == b'*' => match find(t, i + 2, b"*/") {
                Some(p) => {
                    o.put(i, p + 2, Tok::Comment);
                    i = p + 2;
                }
                None => {
                    o.put(i, n, Tok::Comment);
                    return State { kind: J_BLOCK_COMMENT, a: 0, b: 0, ..st };
                }
            },
            _ => i += 1,
        }
    }
    State { kind: 0, a: 0, b: 0, ..st }
}

// ---- line by line: logs, INI files, diffs ----

/// Runs a lexer that only looks at one line at a time over each line of `t`. It gets the line, whether the line
/// starts there (not mid-line) and whether only whitespace came before.
fn by_line(t: &[u8], st: State, o: &mut Out, f: fn(&[u8], bool, bool, &mut Out)) -> State {
    if o.on() {
        let (mut col0, mut bol) = (st.col0, st.bol);
        let mut start = 0;
        loop {
            let end = line_end(t, start);
            f(&t[start..end], col0, bol, &mut o.at(start));
            if end >= t.len() {
                break;
            }
            start = end + 1;
            (col0, bol) = (true, true);
        }
    }
    State { kind: 0, ..st }
}

/// Level words (as whole words), in the spellings logs use: `ERROR`, `error` (nginx, logfmt, JSON logs), `ERR`
/// (Serilog), `fail` and `crit` (.NET), `emerg` and `alert` (syslog)...
const LEVELS: [(&[u8], Tok); 38] = [
    (b"FATAL", Tok::Error),
    (b"Fatal", Tok::Error),
    (b"fatal", Tok::Error),
    (b"FTL", Tok::Error),
    (b"CRITICAL", Tok::Error),
    (b"Critical", Tok::Error),
    (b"critical", Tok::Error),
    (b"CRIT", Tok::Error),
    (b"crit", Tok::Error),
    (b"emerg", Tok::Error),
    (b"alert", Tok::Error),
    (b"ERROR", Tok::Error),
    (b"Error", Tok::Error),
    (b"error", Tok::Error),
    (b"ERR", Tok::Error),
    (b"err", Tok::Error),
    (b"Exception", Tok::Error),
    (b"FAIL", Tok::Error),
    (b"fail", Tok::Error),
    (b"WARNING", Tok::Warn),
    (b"Warning", Tok::Warn),
    (b"warning", Tok::Warn),
    (b"WARN", Tok::Warn),
    (b"warn", Tok::Warn),
    (b"WRN", Tok::Warn),
    (b"INFO", Tok::Info),
    (b"Information", Tok::Info),
    (b"info", Tok::Info),
    (b"INF", Tok::Info),
    (b"notice", Tok::Info),
    (b"NOTICE", Tok::Info),
    (b"DEBUG", Tok::Dim),
    (b"debug", Tok::Dim),
    (b"DBG", Tok::Dim),
    (b"dbug", Tok::Dim),
    (b"TRACE", Tok::Dim),
    (b"trace", Tok::Dim),
    (b"VERBOSE", Tok::Dim),
];

const MONTHS: [&[u8]; 12] = [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];

fn log_line(t: &[u8], col0: bool, _bol: bool, o: &mut Out) {
    let n = t.len();
    let mut i = 0usize;
    if col0 && n > 4 && MONTHS.contains(&&t[..3]) && t[3] == b' ' {
        // a syslog time: `Oct  7 12:00:01`
        let mut j = 4 + t[4..].iter().take_while(|&&b| b == b' ').count();
        j += t[j..].iter().take_while(|b| b.is_ascii_digit()).count();
        let k = j + t[j..].iter().take_while(|&&b| b == b' ').count();
        let e = k + t[k..].iter().take_while(|&&b| b.is_ascii_digit() || b == b':' || b == b'.').count();
        if e > k {
            o.put(0, e, Tok::Dim);
            i = e;
        }
    } else if let Some(e) = ipv4_end(t).filter(|_| col0) {
        // a web server's access log: the client, then the time in brackets (`10.0.0.1 - - [07/Oct/2026:12:00:01 +0000]`)
        if let Some(s) = t[e..n.min(e + 48)].iter().position(|&b| b == b'[').map(|p| e + p) {
            if let Some(c) = t[s..n.min(s + 40)].iter().position(|&b| b == b']') {
                o.put(s, s + c + 1, Tok::Dim);
                i = s + c + 1;
            }
        }
    } else if col0 {
        // A timestamp at the start: digits and date/time punctuation (`T` between a date and a time, `Z` after one:
        // not the T of `TRACE`).
        let mut j = 0;
        let mut digits = 0;
        while j < n.min(40) {
            let b = t[j];
            if b.is_ascii_digit() {
                digits += 1;
            } else if !(matches!(b, b'-' | b':' | b'.' | b',' | b'/' | b' ' | b'[' | b']' | b'+')
                || (b == b'T' && at(t, j + 1).is_ascii_digit())
                || (b == b'Z' && j > 0 && t[j - 1].is_ascii_digit()))
            {
                break;
            }
            j += 1;
        }
        // (not the `[` of `12:00:01 [main]`)
        while j > 0 && matches!(t[j - 1], b' ' | b'[') {
            j -= 1;
        }
        if digits >= 6 && j > 0 {
            o.put(0, j, Tok::Dim);
            i = j;
        }
    }
    // Level keywords as whole words.
    while i < n {
        if t[i].is_ascii_alphabetic() && (i == 0 || !t[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < n && t[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let w = &t[i..j];
            if let Some((_, tok)) = LEVELS.iter().find(|(k, _)| w == *k || (*k == b"Exception" && w.ends_with(b"Exception"))) {
                o.put(i, j, *tok);
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

fn ini_line(t: &[u8], _col0: bool, bol: bool, o: &mut Out) {
    if !bol {
        return;
    }
    let n = t.len();
    let s = t.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(n);
    if s >= n {
        return;
    }
    match t[s] {
        // (`!` starts a comment in Java .properties files)
        b';' | b'#' | b'!' => o.put(s, n, Tok::Comment),
        b'[' => o.put(s, n, Tok::Section),
        _ => {
            // `key = value`, or `key: value` with a one-word key (not `proxy_pass http://…` in nginx.conf, nor an
            // IPv6 address)
            let eq = t[s..].iter().position(|&b| b == b'=').map(|p| s + p);
            let colon = t[s..eq.unwrap_or(n)].iter().position(|&b| b == b':').map(|p| s + p).filter(|&c| {
                c > s
                    && !t[s..c].trim_ascii_end().iter().any(|b| b.is_ascii_whitespace())
                    && !matches!(at(t, c + 1), b'/' | b'\\' | b':')
            });
            let Some(eq) = colon.or(eq) else {
                // a line of words, like a hosts file's `127.0.0.1  localhost  # note`
                let w = s + t[s..].iter().take_while(|b| !b.is_ascii_whitespace()).count();
                if is_num_word(&t[s..w]) {
                    o.put(s, w, Tok::Num);
                }
                if let Some(p) = t[w..].windows(2).position(|x| matches!(x[0], b' ' | b'\t') && matches!(x[1], b'#' | b';')) {
                    o.put(w + p + 1, n, Tok::Comment);
                }
                return;
            };
            o.put(s, eq, Tok::Key);
            o.put(eq, eq + 1, Tok::Punct);
            ini_value(t, eq + 1, o);
        }
    }
}

/// A value after `key =`: a quoted string, number or true/false, and a comment after it.
fn ini_value(t: &[u8], mut i: usize, o: &mut Out) {
    let n = t.len();
    while i < n && (t[i] == b' ' || t[i] == b'\t') {
        i += 1;
    }
    let s = i;
    if matches!(at(t, i), b'"' | b'\'') {
        let q = t[i];
        let (e, _, _) = scan_str(t, i + 1, q, if q == b'"' { b'\\' } else { 0 }, false, true);
        o.put(s, e, Tok::Str);
        i = e;
    } else {
        // the value runs to a comment (" #" or " ;") or the end of the line
        let mut e = s;
        while e < n && !(matches!(t[e], b'#' | b';') && e > s && matches!(t[e - 1], b' ' | b'\t')) {
            e += 1;
        }
        let v = t[s..e].trim_ascii_end();
        let ve = s + v.len();
        if is_num_word(v) {
            o.put(s, ve, Tok::Num);
        } else if matches!(v.to_ascii_lowercase().as_slice(), b"true" | b"false" | b"yes" | b"no" | b"on" | b"off") {
            o.put(s, ve, Tok::Lit);
        }
        i = e;
    }
    // a comment after the value
    if let Some(p) = t[i..].iter().position(|&b| b == b'#' || b == b';') {
        o.put(i + p, n, Tok::Comment);
    }
}

/// Diffs: inside a hunk every line is a removed, added or unchanged one (so `--- x` there is a removed line, not a
/// file header). The state carries how many old lines (`b`) and new lines (`a`, and `mode` for the high byte) the
/// hunk still has, from its `@@ -1,5 +1,6 @@` line.
fn diff(t: &[u8], st: State, o: &mut Out) -> State {
    let mut left = (st.b as u32, st.a as u32 | (st.mode as u32) << 8);
    let mut col0 = st.col0;
    let mut start = 0;
    loop {
        let end = line_end(t, start);
        // (after the text's last line break nothing has started yet: the text that comes next counts that line)
        if col0 && start < t.len() {
            left = diff_line(&t[start..end], left, &mut o.at(start));
        }
        if end >= t.len() {
            break;
        }
        start = end + 1;
        col0 = true;
    }
    let (old, new) = (left.0.min(0xFFFF), left.1.min(0xFFFF));
    State { kind: 0, a: new as u8, b: old as u16, mode: (new >> 8) as u8, ..st }
}

/// Colors one line of a diff; `left` = (old, new) lines still to come in the current hunk.
fn diff_line(t: &[u8], left: (u32, u32), o: &mut Out) -> (u32, u32) {
    let (old, new) = left;
    if old > 0 || new > 0 {
        match t.first() {
            Some(b'-') => {
                o.put(0, t.len(), Tok::Removed);
                return (old.saturating_sub(1), new);
            }
            Some(b'+') => {
                o.put(0, t.len(), Tok::Added);
                return (old, new.saturating_sub(1));
            }
            // unchanged (an empty one when a tool trimmed the space)
            Some(b' ' | b'\r') | None => return (old.saturating_sub(1), new.saturating_sub(1)),
            Some(b'\\') => return left, // "\ No newline at end of file"
            _ => {}                     // anything else: the hunk is over
        }
    }
    let tok = if t.starts_with(b"@@") {
        o.put(0, t.len(), Tok::Section);
        return hunk_lines(t);
    } else if t.starts_with(b"+++") || t.starts_with(b"---") {
        Tok::Heading
    } else if t.starts_with(b"+") {
        Tok::Added
    } else if t.starts_with(b"-") {
        Tok::Removed
    } else if t.starts_with(b"diff ") || t.starts_with(b"index ") || t.starts_with(b"new file") || t.starts_with(b"deleted file") {
        Tok::Keyword
    } else {
        return (0, 0);
    };
    o.put(0, t.len(), tok);
    (0, 0)
}

/// How many old and new lines a `@@ -1,5 +1,6 @@` hunk has (a count left out is 1).
fn hunk_lines(t: &[u8]) -> (u32, u32) {
    let s = String::from_utf8_lossy(t);
    let mut parts = s.split_whitespace().skip(1);
    let mut count = |sign: char| {
        let p = parts.next()?.strip_prefix(sign)?;
        p.split(',').nth(1).map_or(Some(1), |c| c.parse::<u32>().ok())
    };
    match (count('-'), count('+')) {
        (Some(old), Some(new)) => (old, new),
        _ => (0, 0),
    }
}

// ---- CSV ----

/// Each column gets its own color ("rainbow CSV"). Quoted fields may contain separators and line breaks.
/// `a`: 0 at the start of a field, 1 inside an unquoted field, 2 inside quotes, 3 right after a closing quote.
/// `b`: the column.
fn csv(t: &[u8], st: State, sep: u8, o: &mut Out) -> State {
    let mut col = st.b;
    let mut q = st.a;
    let mut field = 0;
    let put = |o: &mut Out, s: usize, e: usize, col: u16| {
        if col % 8 != 0 {
            o.put(s, e, Tok::Col((col % 8) as u8));
        }
    };
    for (i, &b) in t.iter().enumerate() {
        match q {
            2 => {
                if b == b'"' {
                    q = 3;
                }
                continue;
            }
            3 if b == b'"' => {
                // "" inside quotes is a quote character
                q = 2;
                continue;
            }
            0 if b == b'"' => {
                q = 2;
                continue;
            }
            _ => {}
        }
        if b == sep {
            put(o, field, i, col);
            col = col.saturating_add(1);
            field = i + 1;
            q = 0;
        } else if b == b'\n' {
            put(o, field, i, col);
            col = 0;
            field = i + 1;
            q = 0;
        } else if q == 0 || q == 3 {
            q = 1;
        }
    }
    put(o, field, t.len(), col);
    State { kind: 0, a: q, b: col, ..st }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn toks(lang: Lang, text: &str, st: State) -> Vec<(String, Tok)> {
        let mut v = Vec::new();
        lex(lang, text.as_bytes(), st, Some(&mut v));
        v.into_iter().map(|(s, e, t)| (text[s as usize..e as usize].to_string(), t)).collect()
    }

    pub(super) fn has(lang: Lang, text: &str, want: &[(&str, Tok)]) {
        let t = toks(lang, text, State::START);
        for (s, tok) in want {
            assert!(t.contains(&(s.to_string(), *tok)), "{lang:?}: {s:?} as {tok:?} in {text:?}\ngot {t:?}");
        }
    }

    /// The tokens of each line of `text`, each line lexed in the state the view gives it (what everything before it
    /// ends in).
    pub(super) fn view(lang: Lang, text: &str) -> Vec<Vec<(String, Tok)>> {
        let mut st = State::START;
        let mut out = Vec::new();
        for l in text.split_inclusive('\n') {
            out.push(toks(lang, l.trim_end_matches('\n'), st));
            st = lex(lang, l.as_bytes(), st, None);
        }
        out
    }

    /// The state after `text`, lexed in one piece and split at every position, must be the same.
    pub(super) fn end_state(lang: Lang, text: &str) -> State {
        let whole = lex(lang, text.as_bytes(), State::START, None);
        for cut in 1..text.len() {
            if !text.is_char_boundary(cut) || text.as_bytes()[cut - 1] != b'\n' {
                continue;
            }
            let mid = lex(lang, &text.as_bytes()[..cut], State::START, None);
            let end = lex(lang, &text.as_bytes()[cut..], mid, None);
            assert_eq!(end, whole, "{lang:?} split after byte {cut} of {text:?}");
        }
        whole
    }

    #[test]
    fn json_tokens() {
        has(Lang::Json, r#"{"name": "x\"y", "n": -1.5, "ok": true}"#, &[
            ("\"name\"", Tok::Key),
            ("\"x\\\"y\"", Tok::Str),
            ("-1.5", Tok::Num),
            ("true", Tok::Lit),
        ]);
        // continuation of a long line that starts inside a string
        let st = guess(Lang::Json, br#"{"a": 1, "b": "some"#, false);
        let t = toks(Lang::Json, r#"rest of string", "k": 1"#, st);
        assert_eq!(t[0], ("rest of string\"".into(), Tok::Str));
        assert!(t.contains(&("\"k\"".into(), Tok::Key)));
        // a string cut at the segment end continues in the next one
        let st = lex(Lang::Json, br#"{"long": "abc"#, State::START, None);
        assert_eq!(toks(Lang::Json, r#"def": 1"#, st)[0], ("def\"".into(), Tok::Key));
    }

    #[test]
    fn detection() {
        let d = Lang::detect;
        assert_eq!(d(Some("data.JSON"), b""), Lang::Json);
        assert_eq!(d(Some("app.log"), b""), Lang::Log);
        assert_eq!(d(Some("x.txt"), b"  {\"a\":1}"), Lang::Json);
        assert_eq!(d(Some("x.txt"), b"[general]\nkey=1"), Lang::Ini);
        assert_eq!(d(Some("x.txt"), b"[1, 2]"), Lang::Json);
        assert_eq!(d(None, b"hello"), Lang::Plain);
        assert_eq!(d(Some("build.ps1"), b""), Lang::PowerShell);
        assert_eq!(d(Some("Dockerfile"), b""), Lang::Dockerfile);
        assert_eq!(d(Some("web.config"), b""), Lang::Xml);
        assert_eq!(d(Some("notes.md"), b""), Lang::Markdown);
        assert_eq!(d(Some("data.csv"), b"a;b;c\n1;2;3"), Lang::CsvSemi);
        assert_eq!(d(Some("data.csv"), b"a,b,c\n1,2,3"), Lang::Csv);
        assert_eq!(d(Some("script"), b"#!/usr/bin/env python3\nprint(1)"), Lang::Python);
        assert_eq!(d(Some("run"), b"#!/bin/bash\necho hi"), Lang::Shell);
        assert_eq!(d(None, b"<?xml version=\"1.0\"?><a/>"), Lang::Xml);
        assert_eq!(d(None, b"<!DOCTYPE html><html>"), Lang::Html);
        assert_eq!(d(None, b"diff --git a/x b/x\n"), Lang::Diff);
        assert_eq!(d(Some("x.txt"), b"2026-10-07 12:00:01 INFO start\n2026-10-07 12:00:02 WARN slow\n"), Lang::Log);
        assert_eq!(d(Some("readme.txt"), b"Hello there.\nThis is a note.\n"), Lang::Plain);
        // a `[` or `{` start is JSON only when it reads as JSON (also with comments, or cut off by the sample)
        assert_eq!(d(Some("app.txt"), b"[2026-10-07 12:00:01] INFO start\n[2026-10-07 12:00:02] WARN slow\n"), Lang::Log);
        assert_eq!(d(Some("error_log"), b"[07-Oct-2026 12:00:01 UTC] PHP Warning: x\n[07-Oct-2026 12:00:02 UTC] PHP Notice: y\n"), Lang::Log);
        assert_eq!(d(Some("todo.txt"), b"[ ] buy milk\n[x] call mom\n"), Lang::Plain);
        assert_eq!(d(Some("x.txt"), b"{{ jinja }}"), Lang::Plain);
        assert_eq!(d(Some("x.txt"), b"{\n  // comment\n  \"a\": [1, 2"), Lang::Json);
        assert_eq!(d(Some("x.txt"), b"{\"a\":1}\n{\"a\":2}\n"), Lang::Json);
        assert_eq!(d(Some("x.txt"), b"[\"2026-10-07 12:00:01\", 2]"), Lang::Json);
        // the CSV separator counts outside quotes, and tabs count too
        assert_eq!(d(Some("data.csv"), b"\"Name, First\";\"Age\"\n\"x\";1"), Lang::CsvSemi);
        assert_eq!(d(Some("data.csv"), b"a\tb\tc\n1\t2\t3"), Lang::Tsv);
    }

    #[test]
    fn detection_of_the_newer_languages() {
        let d = Lang::detect;
        for (name, lang) in [
            ("Module1.bas", Lang::Vb),
            ("script.ahk", Lang::AutoHotkey),
            ("Cargo.toml", Lang::Toml),
            ("Cargo.lock", Lang::Toml),
            ("nginx.conf", Lang::Nginx),
            (".htaccess", Lang::Apache),
            ("httpd.conf", Lang::Apache),
            ("lib.pm", Lang::Perl),
            ("analysis.R", Lang::R),
            ("main.tf", Lang::Hcl),
            ("CMakeLists.txt", Lang::CMake),
            ("app.properties", Lang::Properties),
            ("movie.srt", Lang::Subtitles),
            ("invite.ics", Lang::Calendar),
            ("App.sln", Lang::Sln),
            // names that said little before
            (".env.local", Lang::Ini),
            ("Jenkinsfile", Lang::Java),
            (".npmrc", Lang::Ini),
            ("values.yaml.j2", Lang::Yaml),
            ("nginx.conf.j2", Lang::Nginx),
            ("page.html.jinja", Lang::Html),
            ("hosts", Lang::Ini),
        ] {
            assert_eq!(d(Some(name), b""), lang, "{name}");
        }
        // a VBA class module, a LaTeX class
        assert_eq!(d(Some("Sheet1.cls"), b"VERSION 1.0 CLASS\r\n"), Lang::Vb);
        assert_eq!(d(Some("article.cls"), b"\\NeedsTeXFormat{LaTeX2e}"), Lang::Plain);
        // nginx and Apache configuration by their folders
        assert_eq!(d(Some("/etc/nginx/sites-available/default"), b"# comment\n"), Lang::Nginx);
        assert_eq!(d(Some("C:\\nginx-1.25.3\\conf\\fastcgi_params"), b"fastcgi_param QUERY_STRING $query_string;\n"), Lang::Nginx);
        assert_eq!(d(Some("C:\\nginx\\html\\index.html"), b""), Lang::Html);
        assert_eq!(d(Some("C:\\nginx\\logs\\error.log"), b""), Lang::Log);
        assert_eq!(d(Some("/etc/nginx/reload"), b"#!/bin/sh\n"), Lang::Shell);
        assert_eq!(d(Some("C:\\work\\nginx-proxy-manager\\NOTES"), b"Some notes.\n"), Lang::Plain);
        // ... its files without an extension only when they read like it (not docs\LICENSE or a README)
        let license = b"Copyright (C) 2002-2021 Igor Sysoev\nAll rights reserved.\n";
        assert_eq!(d(Some("C:\\nginx-1.25.3\\docs\\LICENSE"), license), Lang::Plain);
        assert_eq!(d(Some("/etc/nginx/README"), b"these are the server's settings.\nsee the docs\n"), Lang::Plain);
        assert_eq!(d(Some("/etc/nginx/koi-utf"), b"charset_map  koi8-r  utf-8 {\n    C0  D18E ; # small yu\n"), Lang::Nginx);
        assert_eq!(d(Some("C:\\nginx\\conf\\mime.types"), b"types {\n    text/html  html htm shtml;\n"), Lang::Nginx);
        assert_eq!(d(Some("/etc/apache2/sites-available/000-default.conf"), b"# x\n"), Lang::Apache);
        assert_eq!(d(Some("/opt/apache-maven/bin/mvn"), b"#!/bin/sh\n"), Lang::Shell);
        // ... or by what they hold
        assert_eq!(d(Some("site.conf"), b"server {\n  listen 80;\n}\n"), Lang::Nginx);
        assert_eq!(d(Some("vhost.conf"), b"<VirtualHost *:80>\n  ServerName x\n</VirtualHost>\n"), Lang::Apache);
        assert_eq!(d(Some("fonts.conf"), b"<?xml version=\"1.0\"?>\n<fontconfig/>"), Lang::Xml);
        assert_eq!(d(Some("sysctl.conf"), b"net.ipv4.ip_forward = 1\n"), Lang::Ini);
        assert_eq!(d(None, b"<VirtualHost *:443>\n"), Lang::Apache);
        assert_eq!(d(None, b"<location lat=\"1\"/>"), Lang::Xml);
        assert_eq!(d(Some("run"), b"#!/usr/bin/env perl\n"), Lang::Perl);
        assert_eq!(d(None, b"1\n00:00:01,000 --> 00:00:04,000\nHello\n"), Lang::Subtitles);
        assert_eq!(d(None, b"WEBVTT\n\n00:01.000 --> 00:04.000\nHi\n"), Lang::Subtitles);
        assert_eq!(d(None, b"BEGIN:VCARD\r\nVERSION:3.0\r\n"), Lang::Calendar);
        // a hosts file's addresses aren't log times, but an access log's times are
        assert_eq!(d(Some("list.txt"), b"127.0.0.1 localhost\n10.0.0.2 nas\n"), Lang::Plain);
        let access = b"10.0.0.1 - - [07/Oct/2026:12:00:01 +0000] \"GET / HTTP/1.1\" 200\n10.0.0.2 - - [07/Oct/2026:12:00:02 +0000] \"GET /x\" 404\n";
        assert_eq!(d(Some("access.txt"), access), Lang::Log);
        has(Lang::Ini, "127.0.0.1   localhost  # loopback", &[("127.0.0.1", Tok::Num), ("# loopback", Tok::Comment)]);
        assert!(toks(Lang::Ini, "ff02::1 ip6-allnodes", State::START).is_empty());
        // PPCL by its extension, or by its numbered lines (Desigo exports it as .txt), also cut short
        assert_eq!(d(Some("AHU1.ppcl"), b""), Lang::Ppcl);
        let prog = b"00010     C AIR HANDLER 1\r\n00020     DEFINE(A,\"AHU1.\")\r\n00030     IF(\"%A%SAT\" .GT. 55) THEN GOTO 50\r\n00040     \"%A%SP\" = 55\r\n";
        assert_eq!(d(Some("AHU1_PGM.txt"), prog), Lang::Ppcl);
        assert_eq!(d(Some("AHU1.pcl"), prog), Lang::Ppcl);
        assert_eq!(d(None, &prog[..prog.len() - 7]), Lang::Ppcl);
        assert_eq!(d(None, &prog[..prog.len() - 21]), Lang::Ppcl);
        assert_eq!(d(None, b"00010     C X\n00020     DEFINE(X,\"A.\")\n00030     LOCAL(A)\n000"), Lang::Ppcl);
        // ... also when the first 4 KB are all a header of comments
        let header = "00010     C ---------------------------------------------------------------\n".repeat(70);
        assert_eq!(d(Some("AHU1.txt"), &header.as_bytes()[..4096]), Lang::Ppcl);
        // ... also one that starts with assignments and jumps, short numbers with tabs, lines turned off
        let prog = b"00010     \"A\" = 1\n00020     $LOC1 = \"A\" + 1\n00030     MAX(\"D\",\"A\",$LOC1)\n00040     GOTO 10\n";
        assert_eq!(d(None, prog), Lang::Ppcl);
        let prog = b"10\tC AHU-1 SUPPLY FAN\n20\tIF(TIME .GT. 6.00) THEN ON(\"SFAN\") ELSE OFF(\"SFAN\")\n# 30\tGOTO 10\n";
        assert_eq!(d(Some("x.txt"), prog), Lang::Ppcl);
        // ... but not a numbered list, BASIC, a numbered log, spreadsheet formulas, or a `.pcl` for a printer
        for text in [
            &b"1 C is a language\n2 Rust is too\n3 C\n"[..],
            b"10 PRINT \"HI\"\n20 GOTO 10\n",
            b"10 C = 0\n20 IF (C > 50) THEN PRINT \"BIG\"\n30 GOTO 10\n",
            b"10 IF(X > 5) THEN PRINT \"BIG\"\n20 GOTO 10\n",
            b"1 Set (oven) to 350\n2 Wait (10 min)\n3 Serve\n",
            b"00001 INFO started\n00002 ON (startup) ok\n",
            b"1 IF(A1>5,\"Yes\",\"No\")\n2 SUM(A1:A10)\n",
            b"\x1bE\x1b&l0O text",
        ] {
            assert_eq!(d(Some("x.pcl"), text), Lang::Plain, "{:?}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn detection_of_the_060_languages() {
        let d = Lang::detect;
        for (name, lang) in [
            ("main.dart", Lang::Dart),
            ("App.scala", Lang::Scala),
            ("build.sbt", Lang::Scala),
            ("script.sc", Lang::Scala),
            ("View.mm", Lang::ObjCpp),
            ("part.gcode", Lang::GCode),
            ("part.gco", Lang::GCode),
            ("mill.ngc", Lang::GCode),
            ("mill.cnc", Lang::GCode),
            ("setup.iss", Lang::InnoSetup),
            ("Default.isl", Lang::InnoSetup),
            ("installer.nsi", Lang::Nsis),
            ("macros.nsh", Lang::Nsis),
        ] {
            assert_eq!(d(Some(name), b""), lang, "{name}");
        }
        // .m is Objective-C or MATLAB, .h C or Objective-C, by what they hold
        assert_eq!(d(Some("View.m"), b"//\n//  View.m\n//\n\n#import <UIKit/UIKit.h>\n"), Lang::ObjC);
        assert_eq!(d(Some("plot.m"), b"% plot it\nx = linspace(0, 1);\nfunction y = f(x)\n"), Lang::Plain);
        assert_eq!(d(Some("Greeter.h"), b"@interface Greeter : NSObject\n@end\n"), Lang::ObjC);
        assert_eq!(d(Some("util.h"), b"#include <stdio.h>\nint f(void);\n"), Lang::C);
        assert_eq!(d(Some("util.h"), b"/* @interface in a comment */\n"), Lang::C);
        // .h is C++ too when it reads like it
        for text in [
            &b"#pragma once\n#include <vector>\n"[..],
            b"namespace app {\nclass Widget;\n}\n",
            b"template <class T>\nT f(T a);\n",
            b"struct A {\npublic:\n  int x;\n};\n",
        ] {
            assert_eq!(d(Some("widget.h"), text), Lang::Cpp, "{:?}", String::from_utf8_lossy(text));
        }
        for text in [&b"#include <stdio.h>\n// unlike std::vector\nint f(void);\n"[..], b"/* class Foo\n namespace */\nint classify(int);\n"] {
            assert_eq!(d(Some("util.h"), text), Lang::C, "{:?}", String::from_utf8_lossy(text));
        }
        // .nc is G-code, or NetCDF data
        assert_eq!(d(Some("part.nc"), b"G21\nG90\n"), Lang::GCode);
        assert_ne!(d(Some("ocean.nc"), b"CDF\x01\x00\x00\x00\x00"), Lang::GCode);
        assert_ne!(d(Some("ocean.nc"), "\u{2030}HDF\r\n\u{1a}\n\0\0\0\0\0\u{8}\u{8}\0".as_bytes()), Lang::GCode);
        // G-code by its content: a slicer's header, or lines of G-code words
        assert_eq!(d(Some("print.txt"), b";FLAVOR:Marlin\n;Generated with Cura_SteamEngine 5.4.0\nM140 S60\n"), Lang::GCode);
        assert_eq!(d(None, b"; generated by PrusaSlicer 2.7.1+win64 on 2026-10-08 at 10:02:04 UTC\n\n; external perimeters extrusion width = 0.45mm\n"), Lang::GCode);
        assert_eq!(d(None, b"%\nO1001 (PART)\n(T1 D=6.)\nN10 G90 G94 G17\nN20 G21\nN30 T1 M6\n"), Lang::GCode);
        assert_eq!(d(Some("job"), b"G28\nG1 Z5 F5000\ng1x10y10\nM104 S200\n"), Lang::GCode);
        // ... and not text that only looks a little like it
        assert_eq!(d(Some("roads.txt"), b"M3 is a motorway.\nM4 too\nG7 summit notes\nN1 item\n"), Lang::Plain);
        assert_eq!(d(Some("short.txt"), b"M1\nM2\n"), Lang::Plain);
        assert_eq!(d(Some("x.txt"), b"; settings\n[main]\nkey=1\n"), Lang::Plain);
        assert_eq!(d(Some("list.csv"), b"G1,X,Y\nG2,1,2\nG3,4,5\n"), Lang::Csv);
        assert_eq!(d(None, b"N1,G1,X1\nN2,G1,X2\nN3,G1,X3\n"), Lang::Plain);
    }

    #[test]
    fn comments_in_the_new_languages() {
        use CommentStyle::Line;
        for lang in [Lang::Dart, Lang::Scala, Lang::ObjC, Lang::ObjCpp] {
            assert_eq!(lang.comment(), Some(Line("//")), "{lang:?}");
        }
        for lang in [Lang::GCode, Lang::InnoSetup, Lang::Nsis] {
            assert_eq!(lang.comment(), Some(Line(";")), "{lang:?}");
        }
        // Inno Setup's [Code] is Pascal, commented with `//` (but not its own header, nor the next section's)
        let src = b"[Setup]\nAppName=x\n[Code]\nprocedure A;\n";
        assert!(!inno_code_line(b"[Setup]\n", b"AppName=x"));
        assert!(inno_code_line(&src[..], b"begin"));
        assert!(!inno_code_line(b"[Setup]\n", b"[Code]") && !inno_code_line(&src[..], b"  [Files] "));
    }

    #[test]
    fn the_menu_lists_every_language_once_by_name() {
        let names: Vec<String> = Lang::ALL[1..].iter().map(|l| l.label().to_lowercase()).collect();
        assert_eq!(Lang::ALL[0], Lang::Plain);
        for w in names.windows(2) {
            assert!(w[0] < w[1], "{} before {}", w[0], w[1]);
        }
        // (a language the menu lacks can't be picked, nor colored in a Markdown block)
        for (i, l) in Lang::ALL.iter().enumerate() {
            assert!(!Lang::ALL[..i].contains(l), "{l:?} twice");
        }
    }

    #[test]
    fn new_languages_leave_other_files_alone() {
        let d = Lang::detect;
        // a mind map is XML; Objective-C++ when it reads like code
        assert_eq!(d(Some("ideas.mm"), b"<map version=\"1.0.1\">\n<node TEXT=\"Root\"/>\n</map>\n"), Lang::Xml);
        assert_eq!(d(Some("View.mm"), b"#import <UIKit/UIKit.h>\n"), Lang::ObjCpp);
        // lists of sizes, part numbers or coordinates aren't G-code
        for text in [&b"M3x10\nM3x12\nM4x16\n"[..], b"T100\nT200\nT300\n", b"N40.7128 W74.0060\nN40.7130 W74.0061\nN40.7140 W74.0070\n"] {
            assert_eq!(d(Some("list.txt"), text), Lang::Plain, "{:?}", String::from_utf8_lossy(text));
        }
        assert_eq!(d(Some("part.txt"), b"G21\nG90\nG1 X10 Y10 F1200\nM5\n"), Lang::GCode);
        // a header with `@class` in a comment (C++ by its class), a type library's #import, Octave's #import
        assert_eq!(d(Some("widget.h"), b"/*!\n @class Widget\n */\nclass Widget {};\n"), Lang::Cpp);
        assert_eq!(d(Some("com.h"), b"#import \"msxml6.dll\" rename_namespace(\"x\")\n"), Lang::C);
        assert_eq!(d(Some("data.m"), b"#import data\nx = 1;\n"), Lang::Plain);
        assert_eq!(d(Some("View.h"), b"// a header\n#import <Foundation/Foundation.h>\n@interface View : NSObject\n"), Lang::ObjC);
    }

    #[test]
    fn logs_configs_and_diffs() {
        has(Lang::Log, "2026/10/07 12:00:01 [error] 123#0: open() failed", &[("2026/10/07 12:00:01", Tok::Dim), ("error", Tok::Error)]);
        has(Lang::Log, "time=x level=warn msg=slow", &[("warn", Tok::Warn)]);
        has(Lang::Log, "{\"level\":\"info\"} fail: Microsoft.Hosting [ERR] [DBG]", &[
            ("info", Tok::Info),
            ("fail", Tok::Error),
            ("ERR", Tok::Error),
            ("DBG", Tok::Dim),
        ]);
        has(Lang::Log, "Oct  7 12:00:01 host sshd[1]: Failed", &[("Oct  7 12:00:01", Tok::Dim)]);
        // an access log's time (not its client), and a level right after a time
        let t = toks(Lang::Log, "10.0.0.7 - - [08/Oct/2026:09:00:13 +0000] \"GET / HTTP/1.1\" 200", State::START);
        assert_eq!(t, vec![("[08/Oct/2026:09:00:13 +0000]".into(), Tok::Dim)]);
        has(Lang::Log, "2026-10-08 09:00:14,220 TRACE x", &[("2026-10-08 09:00:14,220", Tok::Dim), ("TRACE", Tok::Dim)]);
        has(Lang::Log, "2026-10-08T09:00:09Z level=warn", &[("2026-10-08T09:00:09Z", Tok::Dim), ("warn", Tok::Warn)]);
        assert!(toks(Lang::Log, "the errors and terrors", State::START).is_empty());
        // `key: value` needs a one-word key: nginx's `proxy_pass http://…` has none
        assert!(toks(Lang::Ini, "    proxy_pass http://backend:8080;", State::START).is_empty());
        assert!(toks(Lang::Ini, "<VirtualHost *:80>", State::START).is_empty());
        has(Lang::Ini, "time: 12:00", &[("time", Tok::Key)]);
        has(Lang::Ini, "path = C:\\x", &[("path ", Tok::Key)]);
        has(Lang::Ini, "! a .properties comment", &[("! a .properties comment", Tok::Comment)]);
        // in a hunk, --- and +++ are a removed and an added line; the hunk's counts say where it ends
        let diff = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n--- old sql comment\n+++ new counter\n context\n--- a/y\n+++ b/y\n";
        let st = end_state(Lang::Diff, diff);
        assert_eq!((st.a, st.b), (0, 0));
        let mut lines = diff.lines();
        let mut st = State::START;
        let mut got = Vec::new();
        for l in lines.by_ref() {
            got.push(toks(Lang::Diff, l, st).first().map(|t| t.1));
            st = lex(Lang::Diff, format!("{l}\n").as_bytes(), st, None);
        }
        use Tok::*;
        assert_eq!(got, [Some(Heading), Some(Heading), Some(Section), Some(Removed), Some(Added), None, Some(Heading), Some(Heading)]);
        assert_eq!(super::hunk_lines(b"@@ -3 +3,0 @@ fn x()"), (1, 0));
    }

    #[test]
    fn huge_json_segments_inside_escaped_json() {
        // a segment starting inside `"payload": "{\"a\":1,…}"` is inside the string
        let rec = r#"{"id":1,"payload":"{\"a\":1,\"b\":[1,2],\"c\":{\"d\":\"e\"}}","n":2},"#;
        let line = rec.repeat(200);
        let (mut in_str, mut esc) = (false, false);
        for (i, b) in line.bytes().enumerate() {
            if i > 4096 && matches!(line.as_bytes()[i - 1], b',' | b'}' | b']' | b' ') {
                let st = guess(Lang::Json, &line.as_bytes()[i - 4096..i], false);
                assert_eq!(st.kind == J_STR, in_str, "at {i}");
            }
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if b == b'"' {
                in_str = true;
            }
        }
    }

    #[test]
    fn log_ini_diff() {
        let t = toks(Lang::Log, "2026-10-07 12:00:01.123 ERROR something failed", State::START);
        assert_eq!(t[0], ("2026-10-07 12:00:01.123".into(), Tok::Dim));
        assert!(t.contains(&("ERROR".into(), Tok::Error)));
        has(Lang::Ini, "  port = 8080", &[("port ", Tok::Key), ("8080", Tok::Num)]);
        has(Lang::Ini, "name = \"Slate\" # the app", &[("\"Slate\"", Tok::Str), ("# the app", Tok::Comment)]);
        has(Lang::Ini, "[server]", &[("[server]", Tok::Section)]);
        has(Lang::Ini, "debug=true", &[("true", Tok::Lit)]);
        has(Lang::Diff, "+added", &[("+added", Tok::Added)]);
        has(Lang::Diff, "@@ -1,2 +1,3 @@", &[("@@ -1,2 +1,3 @@", Tok::Section)]);
    }

    #[test]
    fn csv_columns() {
        let t = toks(Lang::Csv, "a,b,\"c,still c\",d", State::START);
        assert_eq!(t, vec![("b".into(), Tok::Col(1)), ("\"c,still c\"".into(), Tok::Col(2)), ("d".into(), Tok::Col(3))]);
        // a quoted field with a line break keeps its column on the next line
        let st = lex(Lang::Csv, b"x,\"first", State::START, None);
        let t = toks(Lang::Csv, "second\",y", st);
        assert_eq!(t, vec![("second\"".into(), Tok::Col(1)), ("y".into(), Tok::Col(2))]);
        // "" is a quote inside a quoted field
        let t = toks(Lang::Tsv, "1\t\"say \"\"hi\"\"\t!\"\t3", State::START);
        assert_eq!(t[0], ("\"say \"\"hi\"\"\t!\"".into(), Tok::Col(1)));
        end_state(Lang::Csv, "a,\"b\nc\",d\ne,f\n");
    }

    /// Random text that is likely to trip `lang`'s lexer.
    fn tricky_text(lang: Lang, r: &mut u64, len: usize) -> Vec<u8> {
        let common: &[&str] = &[
            " ", "\n", "\t", "x", "1", "-", "=", ":", ";", ",", "(", ")", "{", "}", "[", "]", "<", ">", "/", "*", "\\", "\"", "'", "#",
            "$", "@", "!", "%", "&", "|", "`", "\r\n", "\n\n", "  \n",
        ];
        let extra: &[&str] = match lang {
            Lang::Json => &["//", "/*", "*/", "true"],
            Lang::Xml | Lang::Html => &[
                "<!--", "-->", "<![CDATA[", "]]>", "<?", "?>", "<a", "</a>", "<script>", "</script>", "<style>", "&amp;", "<!DOCTYPE x [", "]>",
            ],
            Lang::Php => &["<?php", "<?=", "?>", "<a href=\"", "<script>", "</script>", "<!--", "//", "/*", "*/", "$x"],
            Lang::Markdown => &[
                "```", "`", "<!--", "-->", "# ", "> ", "- ", "**", "_", "[", "](", "    ", "\n```rust\n", "\n```html\n", "\n~~~sql\n",
                "\n````py\n", "\n```php\n", "<script>", "/*", "*/",
            ],
            Lang::Yaml => &["key: ", "- ", "  ", "|", ">-", "# ", "&a", "!t", "{{ ", "\"", "'", "''", "\\"],
            Lang::Css => &["/*", "*/", "//", "url(", "@media", "#fff", ".c", ":hover"],
            Lang::Python => &["\"\"\"", "'''", "def ", "r\""],
            Lang::Rust => &["r#\"", "\"#", "/*", "*/", "//", "'a", "'\\''", "#["],
            Lang::CSharp => &["@\"", "$\"", "\"\"\"", "/*", "*/", "//", "new ", "X(", "{{", "$\"{"],
            Lang::Lua => &["--[[", "]]", "[==[", "]==]", "--"],
            Lang::PowerShell => &["@\"", "\"@", "@'", "'@", "<#", "#>", "$x"],
            Lang::Batch => &["REM ", "::", ":l", "%%i", "%X%", "echo "],
            Lang::Shell | Lang::Dockerfile => &["${", "<<EOF", "<<-'E'", "\nEOF\n", "\nE\n", "$'", "(("],
            Lang::Ruby => &["<<~EOS", "\nEOS\n", " << ", ":s"],
            Lang::Sql => &["--", "/*", "*/", "''", "[x]", "E'", "-- mysql\n", "/*!"],
            Lang::JavaScript | Lang::TypeScript | Lang::Go => &["${", "/*", "*/", "//", "return ", "/a/", "[/]", "</"],
            Lang::Kotlin | Lang::Swift | Lang::Java | Lang::C => &["\"\"\"", "/*", "*/", "//", "#include <x>"],
            Lang::Cpp => &["R\"(", "R\"x(", ")\"", ")x\"", "1'000", "u8R\"", "/*", "*/", "//", "'a'"],
            Lang::Vb => &["'", "Rem ", "\"\"", "&HFF", "#1/2/2026#", ":", " _", "#If "],
            Lang::AutoHotkey => &["/*", "*/", ";", "::", "^!s::", ":*:btw::", "%x%", "#Requires AutoHotkey v2\n", "`", "Label:"],
            Lang::Toml => &["\"\"\"", "'''", "[a]", "[[b]]", "x = ", "{", "}", "[1, ", "# ", "\\", "1979-05-27 07:32:00", "\"k\" = "],
            Lang::Nginx => &["server {", "location / {", "}", ";", "$host", "${x}", "# c", "a#b", "listen 80;"],
            Lang::Apache => &["<VirtualHost *:80>", "</VirtualHost>", "\\\n", "# c", "%{X}", "[L,R=301]", "${D}", "On"],
            Lang::Perl => &[
                "q(", "qw{", "s/a/b/", "s{", "}{", "tr/", "=~ /", "$#a", "$'", "<<EOF", "\nEOF\n", "\n=head1 x\n", "\n=cut\n",
                "__END__", "m#",
            ],
            Lang::R => &["%in%", "`x y`", "<-", "#'", "is.na(", "...", "\"\n"],
            Lang::Hcl => &["${", "%{", "$${", "<<EOF", "<<-EOT", "\nEOF\n", "/*", "*/", "//", "x = ", "\"a\""],
            Lang::CMake => &["#[[", "]]", "[=[", "]=]", "${X}", "$<A:$<B>>", "$ENV{P}", "if(", "\"\n"],
            Lang::Properties => &["key=", "\\\n", "\\u00e9", "! c", "# c", " : ", "${a}", "\\"],
            Lang::Subtitles => &["WEBVTT\n", "NOTE x\n", "STYLE\n", "::cue { color: red }", "00:00:01.000 --> 00:00:02.000", "<i>", "{\\an8}", "\n\n", "  \n"],
            Lang::Calendar => &["BEGIN:VEVENT", ";TZID=", ":", "\n ", "DTSTART", "\"a:b\""],
            Lang::Sln => &["Project(\"{", "}\") = \"", "GlobalSection(", " = ", "# ", "EndProject"],
            Lang::Ppcl => &[
                "\n00010     ", "\n20\t", "C ", "\"A.B\"", ".EQ.", "IF(", "$LOC1", "@OPER", "01:00", "5.5", "\"$X\"", "\n# 00030 ",
                "UNKNOWN (", "%X%", "\"%X%A\"", ".NOT.", "GOTO ", "OIP(", "DEFINE(", "LOCAL(", "A.ROOT.B",
            ],
            Lang::Diff => &["@@ -1,2 +1,2 @@", "---", "+++", " "],
            Lang::Dart => &["'''", "\"\"\"", "r'", "r\"", "${", "$x", "}", "/*", "*/", "//", "\\"],
            Lang::Scala => &["\"\"\"", "s\"", "raw\"", "${", "$x", "$$", "$\"", "}", "/*", "*/", "'a'", "@main", "\"\"\"\""],
            Lang::ObjC | Lang::ObjCpp => &["@\"", "@interface ", "@end", "#import <x>", "/*", "*/", "//", "@1", "@[", "R\"(", ")\"", "# "],
            Lang::GCode => &["G1 ", "M117 ", "X10.5", "; ", "(", ")", "%", "N10", "#1", "*12", "o100 ", "A=1", "[", "]"],
            Lang::InnoSetup => &[
                "[Code]\n", "[Setup]\n", "[Files]\n", "Name: ", "; ", "{app}", "{#", "#define X ", "{", "}", "(*", "*)", "//", "'", "#13",
                "$FF", "\"\"", "[x] ",
            ],
            Lang::Nsis => &["!include ", "!define ", "$INSTDIR", "${X}", "$(L)", "$\\\"", "$$", "\\\n", "/*", "*/", "/r ", "l:", "Section ", "$"],
            _ => &["[s]", "k=v", "ERROR"],
        };
        let mut s = Vec::new();
        while s.len() < len {
            *r ^= *r << 13;
            *r ^= *r >> 7;
            *r ^= *r << 17;
            let k = (*r % (common.len() + 2 * extra.len()) as u64) as usize;
            s.extend_from_slice(if k < common.len() { common[k] } else { extra[(k - common.len()) % extra.len()] }.as_bytes());
        }
        s
    }

    /// The view works out the state where each line starts by lexing up to it from a checkpoint, and keeps
    /// checkpoints (in very long lines) after a space, comma, ';' or '>'. So lexing a text in two pieces must end
    /// in the same state as lexing it whole: for every language when it's cut after a line break, and for those
    /// whose lexers don't look ahead within a line also when it's cut at those characters.
    #[test]
    fn states_dont_depend_on_where_text_is_cut() {
        use Lang::*;
        let exact_mid_line = [
            Json, Log, Ini, Xml, Html, Csv, CsvSemi, Tsv, Python, JavaScript, TypeScript, C, Cpp, CSharp, Java, Kotlin, Swift, Go,
            Php, Lua, Nginx, Apache, Properties, Calendar, Sln, R, Ppcl, Dart, Scala, ObjC, ObjCpp, GCode,
            Nsis,
        ];
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        for lang in Lang::ALL {
            for _ in 0..150 {
                let len = 10 + (r % 60) as usize;
                let t = tricky_text(lang, &mut r, len);
                let whole = lex(lang, &t, State::START, None);
                for cut in 1..t.len() {
                    let mid_line = matches!(t[cut - 1], b' ' | b',' | b';' | b'>' | b'\t' | b'}');
                    if t[cut - 1] != b'\n' && !(mid_line && exact_mid_line.contains(&lang)) {
                        continue;
                    }
                    let split = lex(lang, &t[cut..], lex(lang, &t[..cut], State::START, None), None);
                    assert_eq!(split, whole, "{lang:?} cut after byte {cut} of {:?}", String::from_utf8_lossy(&t));
                }
            }
        }
    }

    /// Spans may nest (code inside emphasis in Markdown) but never cross: the color of a byte mustn't depend on
    /// which of two spans came last. And coloring a text ends in the state lexing it without colors does (the view
    /// colors each line in the state the lines before end in, worked out without colors).
    #[test]
    fn spans_never_cross() {
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        for lang in Lang::ALL {
            for _ in 0..300 {
                let len = 10 + (r % 80) as usize;
                let t = tricky_text(lang, &mut r, len);
                let mut v = Vec::new();
                let cut = (r as usize % t.len()).max(1);
                let st = lex(lang, &t[..cut], State::START, None);
                let colored = lex(lang, &t[cut..], st, Some(&mut v));
                assert_eq!(colored, lex(lang, &t[cut..], st, None), "{lang:?} after {cut} of {:?}", String::from_utf8_lossy(&t));
                for (k, a) in v.iter().enumerate() {
                    for b in &v[k + 1..] {
                        let crossed = (a.0 < b.0 && b.0 < a.1 && a.1 < b.1) || (b.0 < a.0 && a.0 < b.1 && b.1 < a.1);
                        assert!(!crossed, "{lang:?}: {a:?} and {b:?} cross in {:?}", String::from_utf8_lossy(&t));
                    }
                }
            }
        }
    }

    #[test]
    fn never_panics_on_odd_input() {
        let samples: Vec<Vec<u8>> = vec![
            b"\"".to_vec(),
            b"/*".to_vec(),
            b"<".to_vec(),
            b"<!--".to_vec(),
            b"r#\"".to_vec(),
            b"@\"".to_vec(),
            b"```".to_vec(),
            b"key: |".to_vec(),
            b"\\".to_vec(),
            vec![0xFF, 0xFE, b'"', 0x80, b'\n', b'#'],
            b"--[==[".to_vec(),
            b"'a".to_vec(),
            b"$".to_vec(),
            b"%~".to_vec(),
        ];
        for lang in Lang::ALL {
            for s in &samples {
                let mut v = Vec::new();
                let mut st = lex(lang, s, State::START, Some(&mut v));
                for _ in 0..3 {
                    st = lex(lang, s, st, Some(&mut v));
                    for &(a, b, _) in &v {
                        assert!(a < b && b as usize <= s.len(), "{lang:?} span {a}..{b} of {s:?}");
                    }
                }
            }
        }
    }
}
