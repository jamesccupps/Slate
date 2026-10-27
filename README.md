# Slate

[![Latest release](https://img.shields.io/github/v/release/jamesccupps/Slate?label=release)](https://github.com/jamesccupps/Slate/releases/latest)
[![Build](https://github.com/jamesccupps/Slate/actions/workflows/build.yml/badge.svg)](https://github.com/jamesccupps/Slate/actions/workflows/build.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

A fast, simple text editor for Windows and Linux (Raspberry Pi included) that opens files of any size: an 800 MB
JSON file opens instantly, scrolls smoothly, and can be searched, edited, formatted and saved.

**[⬇ Download for Windows](https://github.com/jamesccupps/Slate/releases/latest/download/Slate.exe)** (one signed
`Slate.exe`, 64-bit Windows 10 or 11) · **[Linux and Raspberry Pi](#linux-and-raspberry-pi)** (apt) ·
[All releases](https://github.com/jamesccupps/Slate/releases) · [Website](https://jamesccupps.github.io/Slate/)

![Slate opening an 810 MB JSON file in 14 ms, jumping to its last line, finding all 435,571 matches of a search in 0.3 s, and showing where the cursor is in a 2-million-item list](docs/images/slate-demo.gif)

<sub>Measured on a Windows PC with an NVMe drive, the file already in Windows' memory cache; the 810 MB file is made-up shop orders.</sub>

- Opens huge files instantly: nothing is loaded up front, only what's on screen is read and drawn. Files open in the
  background, so a slow network drive never freezes the window.
- Tabs that come back after a restart, including unsaved changes (like Windows 11 Notepad), for big files too (on
  Linux, up to 64 MB). Work is never lost silently: anything that can't be kept is asked about, and Windows won't
  shut down over it without asking.
- Find and replace with match case, whole word and regular expressions; fast on huge files. Find in selection
  (Alt+S), the last searches and replacements with Up and Down, and `\n` / `\t` in a regular expression's
  replacement for a line break or a tab.
- Show only the lines that match a search (or the ones that don't), in a tab of their own with each line's number in
  the file; Enter or a double-click goes to that line in the file. 809,820 lines out of a 400 MB log in 0.3 s.
- Bookmarks (Ctrl+F2, then F2 / Shift+F2 to go from one to the next), kept with the tabs; read-only tabs (File → Read
  only; a file you can't write opens that way).
- Syntax colors for 52 kinds of files: Python, PowerShell, Batch, Shell, VBScript/VBA, AutoHotkey, Perl, R, C, C++,
  C#, Objective-C and Objective-C++, Java, Kotlin, Scala, Swift, Dart, Go, Rust, JavaScript, TypeScript, PHP, Ruby,
  Lua, SQL, HTML (with its CSS and scripts), CSS, XML, Markdown (code blocks in their language), YAML, JSON, TOML,
  INI, Java properties, nginx and Apache configuration, Terraform/HCL, CMake, Dockerfiles, Inno Setup and NSIS
  installer scripts, G-code (3D printers, CNC), PPCL programs (Siemens APOGEE and Desigo), CSV with commas or
  semicolons and TSV (each column its own color), logs, diffs, subtitles (SRT, WebVTT), calendars and contacts
  (iCalendar, vCard) and Visual Studio solutions. Block comments and multi-line strings are followed correctly.
- Line tools: sort lines (`file2` before `file10`), remove duplicate or blank lines, trim spaces at line ends;
  UPPERCASE / lowercase / Title Case; comment or uncomment lines with Ctrl+/; duplicate, delete and move lines.
- CSV and TSV: check (every row as many fields as the first, quotes where they may be, with the line and column of
  a mistake; a 300 MB file in about 0.2 s), sort the rows by the caret's column (numbers and dates by value, the
  header row staying on top, rows with line breaks in quotes kept whole), and the column's name in the status bar.
- JSON and XML: format (pretty-print), minify and check, also for JSON Lines; a path bar showing where the caret
  is (`data › [1203] › name`, `catalog › book[3] › title`; copy it as a JSON path or XPath) and a structure panel to
  browse objects, arrays and elements, fast on huge files.
- Print (Ctrl+P): the file or the selection, in its colors, with line numbers (when they're shown) and page numbers;
  long lines go on in the next row, breaking between words. *Microsoft Print to PDF* (Windows) and *Print to File*
  (Linux) make a PDF.
- Characters you can't see are shown: control characters as symbols (␀, ␛…), zero-width ones and a byte order mark
  inside the text as ▯, the bidirectional controls of the "Trojan Source" trick as ⇄ (they don't reorder the text on
  screen); Show whitespace marks non-breaking and other look-alike spaces. *View → Character at the caret* says
  what one is (`é  U+00E9  ·  UTF-8: C3 A9`).
- From a command line: `slate notes.txt:120` opens a file at a line, `slate --read-only file` opens it read only,
  `slate --wait file` returns once its tab is closed (`git config --global core.editor "slate --wait"`), and
  `some-command | slate -` opens what it prints in a new tab.
- Go to line, word wrap, line numbers, show whitespace, the matching
  bracket highlighted, overtype, zoom, dark and light themes (follows Windows, or switch with the sun/moon button).
  With Windows' high contrast on, Slate uses its colors, and Magnifier follows the text cursor.
- Reopen closed tabs (Ctrl+Shift+T), close the other, the saved or all tabs, and a list of all tabs when they don't
  fit. The files opened last are on the taskbar button's menu (on Linux, in the desktop's recent files). The status bar counts words and characters, and shows the language, line endings, encoding and indentation:
  click one to change it.
- Detects and keeps the encoding (UTF-8, UTF-8 with BOM, UTF-16, ANSI) and line endings (CRLF / LF). Saving as ANSI
  asks first if some characters would turn into "?".
- Notices when another program changes an open file; reloads it automatically when you have no unsaved changes
  (handy for log files).
- A single small `Slate.exe` that runs from any folder. *Help → Open files with Slate…* adds it to "Open with",
  the Start menu and the right-click menu; *Help → Stop opening files with Slate…* takes that back. Scripts that
  Windows runs when they're double-clicked (`.bat`, `.cmd`, `.vbs`, `.js`, `.reg`, `.py`…) are left alone: open
  them with *Edit with Slate* on the right-click menu.
- Keeps itself up to date: at most once a day it asks GitHub for the latest release, and when there's a newer one
  an *Update* button appears in the status bar. One click downloads it, checks it against the release's checksum
  (and, from 0.8.1, that it's signed by the same publisher) and restarts Slate with your tabs (and their unsaved
  text) back where they were. If the new version can't start, the old one comes back by itself. *Help → Check for
  updates…* does it on demand. (On Linux, `apt` keeps Slate up to date.)
- Screen readers on Windows (Narrator, NVDA, JAWS) read the text through UI Automation: the line, word or character
  at the cursor as it moves, the selection and edits, and the tab's name when another tab gets the keyboard; huge
  files too. Not yet: the tab strip, status bar and menu bar, and Linux (Orca)
  ([docs/ROADMAP.md](docs/ROADMAP.md)).

## Keyboard shortcuts

The usual ones work as in Notepad (Ctrl+N, O, S, F, H, G, Z, Y, A, X, C, V), plus:

| Keys | |
| --- | --- |
| Ctrl+W / Ctrl+Shift+T | Close tab / reopen the tab closed last |
| Ctrl+Tab, Ctrl+1 … Ctrl+9 | Next tab, go to a tab |
| Ctrl+Alt+S | Save all |
| Ctrl+P | Print |
| Ctrl+D / Ctrl+Shift+K | Duplicate / delete the line |
| Alt+Up / Alt+Down | Move the line up / down |
| Ctrl+/ | Toggle comment |
| Ctrl+U / Ctrl+Shift+U | lowercase / UPPERCASE |
| Shift+Alt+F | Format JSON or XML |
| F3 / Shift+F3 | Next / previous match |
| Alt+L | Show only the lines with a match |
| Alt+S (in the find box) | Find in selection |
| Ctrl+F2 / F2 / Shift+F2 | Bookmark the line / next / previous bookmark |
| Alt+Z | Word wrap |
| Ctrl+Plus / Ctrl+Minus / Ctrl+0 | Zoom in / out / reset |
| Ctrl+Shift+O | JSON and XML structure panel (Windows) |
| F5 | Insert the time and date |

*Help → Keyboard shortcuts* lists them all. On Linux, Alt+1 … Alt+9 go to a tab too.

## Download

Get `Slate.exe` from the [latest release](https://github.com/jamesccupps/Slate/releases/latest) and run it from
any folder. It needs 64-bit Windows 10 or 11. `Slate.exe --install` sets it up for your account without asking
anything (what *Help → Open files with Slate…* does, for scripts and package managers).

`Slate.exe` is code-signed (publisher: James Cupps, through Microsoft's Artifact Signing) since 0.8.0. While the
certificate is new, Windows' SmartScreen may still show "Windows protected your PC" the first time; choose *More
info* → *Run anyway*. Each release lists the SHA-256 of `Slate.exe` (in `Slate.exe.sha256`) if you want to check
the download.

### Linux and Raspberry Pi

Slate runs on 64-bit Linux with GTK 4.8 or newer: Debian 12, Raspberry Pi OS 12 (Bookworm, on a Raspberry Pi 4 or
5), Ubuntu 24.04 and newer, or similar. To install it, run this line in a terminal; it gets the package for your
computer from the latest release (into a new temporary file of your own) and installs it only if that worked:

```
f=$(mktemp --suffix=.deb) && wget -O "$f" https://github.com/jamesccupps/Slate/releases/latest/download/slate-linux-$(dpkg --print-architecture).deb && chmod 644 "$f" && sudo apt install "$f"
```

Slate is built for 64-bit systems only (`amd64` and `arm64`): on 32-bit Raspberry Pi OS (`dpkg --print-architecture`
says `armhf`) the download stops with "404 Not Found".

Or add Slate's apt repository yourself and install it by name, like any other package:

```
sudo wget -O /usr/share/keyrings/slate-archive-keyring.gpg https://jamesccupps.github.io/Slate/apt/slate-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/slate-archive-keyring.gpg] https://jamesccupps.github.io/Slate/apt stable main" | sudo tee /etc/apt/sources.list.d/slate.list
sudo apt update && sudo apt install slate
```

![Slate on Linux (GTK 4): a Python file, light theme](docs/images/slate-linux.png)

Slate is then in the menu (Accessories), opens files from the file manager, and `slate notes.txt` opens a file from
a terminal; **Help → Open files with Slate…** makes it the app that opens text, Markdown, CSV, JSON, XML, YAML, log
and code files when they're double-clicked. The package also adds Slate's own apt repository (signed; `/etc/apt/sources.list.d/slate.list`), so
`sudo apt upgrade` keeps Slate up to date along with the rest of the system; removing Slate removes that too. The
[latest release](https://github.com/jamesccupps/Slate/releases/latest) also has the packages to download yourself
(`slate-linux-arm64.deb` for 64-bit ARM like the Pi, `slate-linux-amd64.deb` for PCs; install one with
`sudo apt install ./slate-linux-arm64.deb`) and `.tar.gz` files with the same program to run without installing
(those don't update). Its settings, session and `crash.log` are in `~/.local/share/slate`.

The Linux version has the same engine, file handling, colors, find and replace, line tools and JSON/XML tools as on
Windows, in a GTK window. Not on Linux yet:

- the JSON/XML path bar and structure panel;
- show whitespace, the matching bracket highlighted, overtype, high contrast and the theme button;
- word and character counts, and changing the encoding or indentation from the status bar;
- the Encoding, Reopen with encoding, Indentation, Font and JSON indentation menus (the font, the indentation and
  JSON's are set in `settings.json`);
- Close other / saved / all tabs, the list of all tabs, the tab's own menu, Reload, Show in folder and Copy file
  path, and clearing the Open recent list;
- *File → Restore last session* (`restore_session` in `settings.json`), *Help → Open settings folder*, *Help →
  Report a problem…* and crash dumps;
- keeping unsaved changes to documents over 64 MB between runs (closing asks about them instead).

## Where Slate keeps things

- Settings, the session (open tabs and unsaved text) and `crash.log` are in `%LOCALAPPDATA%\Slate`
  (*Help → Open settings folder*; on Linux `~/.local/share/slate`). *File → Restore last session* (Windows; on Linux
  `restore_session` in `settings.json`) turns the session off: closing then asks about unsaved changes, like Notepad
  used to.
- **Portable mode:** put an empty file named `Slate.portable` next to `Slate.exe`, and Slate keeps all of that, and
  its temporary files, in a `data` folder beside it instead (handy on a USB stick or a fast drive).
- Slate running as administrator keeps its own session, apart from the normal one.

## Updates and privacy

The update check is one request to `api.github.com` for the latest release of this repository; it sends nothing
about you or your files. Turn it off with *Help → Check for updates automatically*. Slate doesn't send anything
else anywhere. If it crashes, it writes `crash.log` (and on Windows a small crash dump) in its settings folder, and
only there; *Help → Report a problem…* (Windows) opens a GitHub issue form with the version filled in, which you can
edit and send yourself, attaching those files if you like.

## Uninstalling

To keep Slate but stop it opening files when they're double-clicked: *Help → Stop opening files with Slate…*; the
file types you set to open with Slate go back to Windows' own choice. To remove it: if you used *Open files with
Slate…* (or winget), Windows Settings → Apps → Installed apps → Slate → Uninstall removes it. Otherwise just delete
`Slate.exe`. Your settings and unsaved text stay in `%LOCALAPPDATA%\Slate` (or the portable `data` folder): delete
that too if you don't need them any more. On Linux: `sudo apt remove slate`.

## License

MIT: see [LICENSE](LICENSE). The libraries built into `Slate.exe` are listed, with their licenses, in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Building

Windows: needs Rust (the GNU toolchain is enough; no Visual Studio). Run `build.cmd`, which produces
`dist\Slate.exe`. Linux: Rust and GTK 4's development files (`sudo apt install build-essential libgtk-4-dev`), then
`cargo build --release` (`target/release/Slate`); `packaging/linux/package.sh` makes the `.deb`.
`cargo test --lib` runs the unit tests, and `Slate.exe --test tests\smoke.txt` drives the real app in a hidden
window (set `SLATE_DATA_DIR` to an empty folder first: the smoke test writes a session there and reads it back).
GitHub Actions builds and tests every push to `main`, every pull request and every version tag, for Windows and for
Linux on x86-64 and ARM64 (on Debian 12, under Xvfb:
`tests/smoke-linux.txt`); a version tag (`v1.2.3`) drafts a release with `Slate.exe`, `Slate.exe.sha256` and the
Linux packages attached. What's planned is in [docs/ROADMAP.md](docs/ROADMAP.md).
