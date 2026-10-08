# Slate

A fast, simple text editor for Windows that opens files of any size: an 800 MB JSON file opens instantly, scrolls
smoothly, and can be searched, edited, formatted and saved.

- Opens huge files instantly: nothing is loaded up front, only what's on screen is read and drawn.
- Tabs that come back after a restart, including unsaved changes (like Windows 11 Notepad). Work is never lost
  silently: anything that can't be kept is asked about, and Windows won't shut down over it without asking.
- Find and replace with match case, whole word and regular expressions; fast on huge files.
- Syntax colors for about 30 kinds of files: Python, PowerShell, Batch, Shell, C, C++, C#, Java, Kotlin, Swift, Go,
  Rust, JavaScript, TypeScript, PHP, Ruby, Lua, SQL, HTML (with its CSS and scripts), CSS, XML, Markdown, YAML,
  JSON, INI/TOML, CSV/TSV (each column its own color), logs, diffs and Dockerfiles. Block comments and multi-line
  strings are followed correctly.
- Line tools: sort lines (`file2` before `file10`), remove duplicate or blank lines, trim spaces at line ends;
  UPPERCASE / lowercase / Title Case; comment or uncomment lines with Ctrl+/.
- JSON and XML: format (pretty-print), minify and check, also for JSON Lines; for JSON a path bar showing where
  the caret is (`data › [1203] › name`) and a structure panel to browse objects and arrays, fast on huge files.
- Go to line, word wrap, line numbers, zoom, dark and light themes (follows Windows, or switch with the sun/moon
  button).
- Detects and keeps the encoding (UTF-8, UTF-8 BOM, UTF-16, ANSI) and line endings (CRLF / LF).
- Notices when another program changes an open file; reloads it automatically when you have no unsaved changes
  (handy for log files).
- A single small `Slate.exe` that runs from any folder. *Help → Open files with Slate…* adds it to "Open with",
  the Start menu and the right-click menu.
- Keeps itself up to date: at most once a day it asks GitHub for the latest release, and when there's a newer one
  an *Update* button appears in the status bar. One click downloads it, checks it against the release's checksum
  and restarts Slate with your tabs (and their unsaved text) back where they were. If the new version can't start,
  the old one comes back by itself. *Help → Check for updates* does it on demand.

## Download

Get `Slate.exe` from the [latest release](https://github.com/jamesccupps/Slate/releases/latest) and run it from
any folder. It needs 64-bit Windows 10 or 11.

Slate isn't code-signed yet, so the first time you run it Windows may show "Windows protected your PC"; choose
*More info* → *Run anyway*. Each release lists the SHA-256 of `Slate.exe` (in `Slate.exe.sha256`) if you want to
check the download.

## Where Slate keeps things

- Settings, the session (open tabs and unsaved text) and `crash.log` are in `%LOCALAPPDATA%\Slate`.
- **Portable mode:** put an empty file named `Slate.portable` next to `Slate.exe`, and Slate keeps all of that, and
  its temporary files, in a `data` folder beside it instead (handy on a USB stick or a fast drive).
- Slate running as administrator keeps its own session, apart from the normal one.

## Updates and privacy

The update check is one request to `api.github.com` for the latest release of this repository; it sends nothing
about you or your files. Turn it off with *Help → Check for updates automatically*. Slate doesn't send anything
else anywhere. If it crashes, it writes `crash.log` (and a small crash dump) in its settings folder, and only
there; *Help → Report a problem…* opens a GitHub issue form with the version filled in, which you can edit and send
yourself, attaching those files if you like.

## Uninstalling

If you used *Open files with Slate…*: Windows Settings → Apps → Installed apps → Slate → Uninstall. Then delete
Slate's folder, and `%LOCALAPPDATA%\Slate` (or the portable `data` folder) if you don't need your settings and
unsaved text any more. Otherwise just delete `Slate.exe`.

## License

MIT: see [LICENSE](LICENSE). The libraries built into `Slate.exe` are listed, with their licenses, in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Building

Needs Rust (the GNU toolchain is enough; no Visual Studio). Run `build.cmd`, which produces `dist\Slate.exe`.
`cargo test --lib` runs the engine's tests, and `Slate.exe --test tests\smoke.txt` drives the real app offscreen.
GitHub Actions builds and tests every push; a version tag (`v1.2.3`) drafts a release with `Slate.exe` and
`Slate.exe.sha256` attached. What's planned is in [docs/ROADMAP.md](docs/ROADMAP.md).
