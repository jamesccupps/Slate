# Slate roadmap

Where Slate is going, in order. The order follows what Slate is for: **never lose the user's work**, then be right,
then be fast, then add features, and only the features people actually use. Sizes: **S** = up to half a day,
**M** = one to three days, **L** = more.

Unsorted ideas that aren't planned yet live in [IDEAS.md](IDEAS.md).

## 0.3.0: the audit release

In October 2026 the whole app was audited in four parts (the engine; formats and coloring; the editor and window;
updates, setup and docs), and everything that could lose work, write the wrong bytes or get in the way was fixed.
The details are in the [0.3.0 release notes](release-notes-0.3.0.md). In short:

- **Unsaved work:** the session can't lose text any more (lenient reading, flushed writes, only Slate's own backups
  deleted, stray backups come back as tabs, a crash while restoring is caught), and it's written in the background.
- **Files:** a big file another program rewrites isn't saved mixed with the user's edits; lossy ANSI saves ask first;
  mostly-UTF-8 files stay UTF-8 byte for byte; read errors never become wrong line numbers; saving works on network
  shares and FAT drives; disk checks run in the background.
- **Editing:** per-file indentation, whole-character caret moves, line-wise paste, one-step undo when typing over a
  selection, instant bulk indent/comment, AltGr-safe shortcuts, the Alt menu bar, touchpad-friendly scrolling.
- **Formats:** XML keeps meaningful whitespace; JSON with comments and cut-short JSON get a structure; colors stay
  right after stray quotes, in heredocs, PHP pages and MySQL dumps; fast on pathological lines.
- **Updates and CI:** the old version comes back if a new one can't start; hourly checks; CI checks the tag against
  the version, smoke-tests the built exe and fills in the release notes.

## 0.4.0

What the plan's first round added: unsaved changes to big files (over 64 MiB) survive closing; tabs whose network
share doesn't answer wait and retry; files open and reload on another thread; high contrast and a system caret for
Magnifier; show whitespace, matching brackets, reopen closed tab, close all / close saved, overtype, word and
character counts, a list of tabs, `file:line`; 13 more languages and better detection; Markdown code blocks in
their language; an XML path bar and structure panel; crash reports without telemetry; the MIT license. Details in the
[0.4.0 release notes](release-notes-0.4.0.md).

## 0.5.0

PPCL, the language of Siemens APOGEE and Desigo field panels, built against real Desigo exports and the command
lists of Siemens' manual: colored much as Desigo's editor shows it, with the mistakes that need no parsing in red
(line numbers out of order, open quotes, a `GOTO` without a line number, operators PPCL hasn't got), recognized by
its numbered lines in the `.txt` files Desigo exports (and as `.ppcl`, or a `.pcl` that reads like it), and Toggle
comment puts the `C` after each line's number. Prompts follow dark mode. Details in the
[0.5.0 release notes](release-notes-0.5.0.md).

## 0.6.0

Seven more languages: Dart, Scala, Objective-C and Objective-C++, G-code, Inno Setup (with Pascal Script in its
`[Code]` section) and NSIS, none slower than C. Details in the [0.6.0 release notes](release-notes-0.6.0.md).

## 0.7.0: speed and polish

The second audit, in five parts (the engine; colors; the text view; the window around it; starting, the session and
updates), weighted to speed and polish. Text is drawn by Slate's own renderer, only what's in view: long lines
packed with colors draw 10 to 100 times faster, and memory stays low while paging through huge ones. Coloring code is
about twice as fast, ANSI and UTF-16 files open several times faster. Search finds regex matches across its steps
exactly, and runs regexes in the background. Crisp lines at 125% and 150%, tooltips, pressed states, a scrollbar in
the structure panel, a hanging indent with word wrap. Details in the [0.7.0 release notes](release-notes-0.7.0.md).

## 0.8.0: Linux

Slate on Linux, Raspberry Pi included: the same engine, colors and editing operations (now shared: `src/core`,
`src/highlight`, `src/edit.rs`, `src/theme.rs`, `src/settings.rs`, with the engine's few system calls behind
`core::os`) in a GTK 4 window (`src/gtk`), built for x86-64 and ARM64 on Debian 12 and packaged as a `.deb`. Huge files
open as fast as on Windows; colors are drawn with the glyphs, only what's in view. Details in the
[0.8.0 release notes](release-notes-0.8.0.md).

## 0.9.0: the third audit

An outside audit (October 2026) of both versions, checked finding by finding. The Linux version's session now follows
Windows' rules (a session file it can't read or write never costs unsaved text; written on another thread; logging
out and shutting down write it first), a file that can't be opened no longer ends it, and its menus, shortcuts and
commands match Windows' (Line endings for the whole text, Format by language, Open recent). On both: saves keep a
file's permissions, encryption, streams and extended attributes; a same-length rewrite by another program is
noticed; absurdly deep JSON or XML and huge Format results no longer use gigabytes; `file:line:col` works on Windows;
searching JSON for a key and value is 3-5 times faster, sorting about twice as fast, typing with the path bar on no
longer rescans the file per key. Details in the [0.9.0 release notes](release-notes-0.9.0.md).

## 0.9.1

The audit's remaining release and update findings, and the speed 0.9.0 lost in two places. Updates check the
signing identity as well as the name; a second Slate hands its files only to a Slate running as the same user with
the same rights; on Linux the menu actions aren't published on D-Bus; the apt repository's index expires a year after
signing and is signed again monthly; the workflows' actions are pinned to commits. Counting and Replace all in big
files are about 10% faster than in 0.8.2 (0.9.0 had made them 5% slower), and searches like `Wool socks` are back to
full speed. Details in the [0.9.1 release notes](release-notes-0.9.1.md).

## 0.10.0: Print

File → Print (Ctrl+P) on both: the system's print dialog, the file or the selection in its colors, line numbers and
page numbers, long lines breaking between words; up to 10,000 pages (asked about past 100). Also what an outside
recheck of 0.9.1 found: Linux logging out with a session it can't write asks about unsaved tabs, Save as UTF-16 no
longer ends in an ANSI save, a line-ending change keeps the caret on its line, Save As on Linux, the scrollbar's
marks, uninstalling right before a reinstall, and the apt repository checked as apt checks it before it's published;
a pretty-printed JSON's structure a third faster. Details in the [0.10.0 release notes](release-notes-0.10.0.md).

## 0.11.0: the lines that matter

Show only matching lines (Alt+L, or the lines that don't match): a tab of their own, read only, numbered as in the
file, Enter or a double-click going to the line there; filtering such a tab again keeps the file's numbers. 809,820
lines out of a 400 MB log in 0.3 s. Bookmarks (Ctrl+F2, F2, Shift+F2), kept with the tabs. Read-only tabs, on by
themselves for files that can't be written. Details in the [0.11.0 release notes](release-notes-0.11.0.md).

## 0.12.0: screen readers, and a security review

Screen readers on Windows read the text (UI Automation: `src/ui/uia.rs`): the line, word or character at the caret,
the selection, edits and the tab's name, on huge files too. A security review's fixes: Slate.exe takes its DLLs from
System32 only, saving keeps a file's own permissions in every case, and the parsers were fuzzed (14,000 inputs, no
crash or hang). Details in the [0.12.0 release notes](release-notes-0.12.0.md).

## 0.12.1: polish

A review of every screen on Windows and Linux (light, dark and high contrast; 100% to 200%; windows down to the
smallest size), and what it found fixed: the tab strip keeps the tab you're on in view, the accent color in the shade
Windows 11's apps use on each theme, status bar messages that went away too late (or, on Linux, never), the find bar's
matches following the tab on Linux. Details in the [0.12.1 release notes](release-notes-0.12.1.md).

## 0.12.2: the fourth audit

Check as strict as JSON and XML are (UTF-8 and one value per line; XML's characters, character references and the
declaration's place), with the characters checked on a second thread so it takes no longer; saving leaves alone a
file you aren't allowed to change; no update by itself where Windows can't check Slate's signature; names that aren't
valid Unicode kept; the apt repository without an expiry date; the .deb with checksums, a Debian-format copyright and
a changelog. Details in the [0.12.2 release notes](release-notes-0.12.2.md).

## 0.12.3: quicker to open, lighter on big files

The window no longer waits for a slow graphics driver: its first frames are drawn in software (WARP) until the card's
device is ready (on an NVIDIA PC the first frame at ~90 ms instead of ~250 ms), and it comes on the screen with its
first frame drawn, without the white flash it always had. Sort and the other line tools write their result as they
go (a 400 MB log sorted in 2.8 s instead of 4.5 s, in 490 MB instead of 830 MB). An idle Slate on Linux sleeps.
Details in the [0.12.3 release notes](release-notes-0.12.3.md).

## 0.12.4

Windows: a tab of a file opened from inside a zip (Explorer's temp folder, cleaned up later) no longer waits for ever
after a restart: a folder gone from the PC's own disk counts as gone. "Open files with Slate…" says that Windows asks
once per kind of file, and afterwards which usual kinds open with Slate. Details in the
[0.12.4 release notes](release-notes-0.12.4.md).

## 0.12.5

A review of the whole program: Save as… and Open… for a new document start in the last folder used (or Documents);
Windows' tab strip shows whole tabs only; Keyboard shortcuts takes the place of an empty new tab; Copy path says when
the clipboard couldn't be used; Linux keeps the tab shown when a file can't be opened, and in a narrow window keeps
the line and column in the status bar and room for the search box. Details in the
[0.12.5 release notes](release-notes-0.12.5.md).

## Next

1. **Accessibility: screen readers, the rest (M).** The text is read since 0.12.0 (checked in test mode and by a real
   UI Automation client, `tests/uia-client.ps1`). Next:
   - Trying it with Narrator and NVDA (focus events can only be checked in a real, focused window).
   - Notifications (`UiaRaiseNotificationEvent`, Windows 10 1709+) for what the status bar says ("Saved", "Not
     found", a search's count) and for the line a Find next lands on while the find box has the keyboard.
   - The tab strip (a Tab with its tab items, selectable), the status bar (a StatusBar: NVDA reads it with
     NVDA+End) and the menu bar (its titles as menu items while Alt has it; the menus are Windows' own and read already).
   - The JSON and XML path bar and structure panel (a tree), and Linux: Orca needs GTK 4.14's `GtkAccessibleText`
     for a view Slate draws itself (Debian 12 and Raspberry Pi OS 12 have GTK 4.8).
2. **Release basics (S–M).** Slate.exe is signed since 0.8.0 (Azure Artifact Signing, in CI); since 0.8.1 the
   updater checks that an update is signed by the same publisher, and `Slate.exe --install` installs silently for
   winget (`JamesCupps.Slate`, submitted with 0.8.1 and moved to 0.8.2 before it was merged). Next: a winget
   manifest PR made by CI for each release (needs a GitHub token of the user's as a secret), Scoop and Chocolatey.
   (Licensed MIT since October 2026.)
3. **Big unsaved files, the rest (S–M).** Keep a big document's newline index in the session so putting it back
   doesn't count the lines again, and keep big documents that Format or Replace All rewrote (today closing still asks
   about those). (Its `.data` is already written anew, with only what the text uses, once most of it isn't.)

## Soon

- **CSV and TSV** (S–M; the user asked, October 2026). Today they're plain text (colored, nothing more). **Check**
  as for JSON and XML: a row with another number of fields than the header, a quote left open, a stray quote inside
  a field, said with its line and column; the quoting as RFC 4180 has it (a field in quotes may hold the separator,
  `""` and line breaks), the separator found from the first lines (`,` `;` tab `|`). **Sort by this column** (Format →
  Sort lines by the caret's column: the header row stays on top, numbers and dates sorted as such, rows with line
  breaks in quotes kept whole). The caret's column by its name in the status bar (`status`, field 9). All streaming
  like the rest (the aim: a 400 MB CSV checked in about the time a search of it takes, ~0.2 s).
- **Matching lines, more** (S each): a button in the find bar; the tab of lines following a log as it grows (filtering
  what's added); several searches at once (lines with any of them, highlighted each in a color).
- `slate --read-only` from a command line (S: the files handed to a Slate that's running already would need it
  too).
- **The character at the caret** (S): its code point and bytes (`U+00E9 é · C3 A9`) from the status bar's encoding.
- **Invisible and look-alike characters shown** (S): zero-width and non-breaking spaces, a byte order mark inside the
  text, and the bidirectional controls of the "Trojan Source" trick, as symbols, as control characters are already.
- **`slate --wait` and standard input** (S): `--wait` returns when the tab closes (Slate as git's editor);
  `some-command | slate -` opens the output in a new tab.
- **Page setup** (M): margins, the text at the top and bottom of the pages, printing without colors (Print itself
  came in 0.10.0).
- **Print preview** (M; the user asked, October 2026): File → Print preview… opens Slate's own window with the pages
  as they'll print (`src/print.rs`'s layout, drawn with Direct2D): page arrows, the page count, zoom, and Print, which
  goes on to the system's dialog. The same on Windows 10 and 11 with either print dialog. (Windows 11's newer print
  dialog says "This app doesn't support print preview" for every classic app that prints the usual way, as Slate
  does; filling its preview would mean Windows' newer printing method, whose preview Microsoft documents for Store apps
  only. The older dialog never had a preview.) On Linux, check GTK's print dialog first: its Preview button may already
  work with Slate's `GtkPrintOperation`.
- **Find:** in the selection, a history, `\n` and `\t` in regex replacements, find in all open tabs (S–M).
- **Editing:** column (Alt+drag) selection (S). Drag and drop of text (M).
- **Spell check** like Notepad's (M). **Inline IME composition** for Chinese, Japanese and Korean (M).
- **Tabs in the title bar**, like Windows 11 Notepad, to save space (M).
- **A taskbar jump list** with the recent files (S).
- **JSON structure panel:** keep an open array element open when elements are inserted before it (it's keyed by
  index today); while the panel is updating after an edit, clicks go to the row's old place (S).
- **Count all and Show only matching lines on several cores (M).** Both search a big file on one thread: 850 MB in
  0.35–0.45 s (a plain query is searched at several GB/s; reading the file from Windows' cache goes at 6 GB/s, faster
  still on more threads). Split into ranges searched side by side, the counts added (and the first 10,000 places kept
  in order), it could take 0.1–0.15 s. Show only matching lines is harder: each part's lines and line map written
  apart, then joined in order (a big result goes to a temp file).
- **Sort and Remove duplicates, faster (M).** On a 400 MB log of 4.85 million lines, Sort takes 2.8 s and Remove
  duplicates 1.1 s (October 2026). Sort compares lines the way people sort (numbers by value, case ignored), on up to
  8 threads, but its last merge runs on one: merging in parallel (splitting the runs where they meet) would save
  ~0.5 s, and a key of each line's first bytes kept beside it would answer most comparisons without reading the text.
  Remove duplicates is held up by memory (a faster hash saved only 5–20%): split by hash across threads, each with a
  table of its own, it could be ~3 times faster.

## Later

- **Linux, the rest (M each unless said):** the JSON/XML structure panel and path bar; keeping big unsaved documents in the session
  (`.pieces`/`.data`, as on Windows); show whitespace, bracket matching, overtype, word counts and the tab list (S
  each); the status bar's encoding and indentation menus, the Encoding, Indentation and Font menus, Close other /
  saved / all tabs and the tab's menu, Reload, Show in folder, Copy file path (S each); following GNOME's dark/light
  switch while running (S); Flatpak or AppImage for distributions without GTK 4.8 (S–M); an accessibility pass with
  Orca (M; GTK 4.8 has no accessible text for a custom-drawn view: GTK 4.14's `GtkAccessibleText`).
- **Maybe: a Markdown preview** (a rendered view next to the text, off by default) (M). Only if people ask for
  it: Slate is meant to stay quick and open anything, so it must cost nothing while it isn't used.
- JSON and XML: **go to a path** typed in Go to line (`data[1203].name`, `/catalog/book[3]/title`) (S).
- **EditorConfig** (`.editorconfig`: indentation, line endings, a final newline, the encoding) (S).
- **A command palette** (Ctrl+Shift+P) over every command and setting (M; on Linux it would also be the settings
  window it hasn't got).
- **Two views of one document**, one above the other (M). **Compare** with the file as saved, or two tabs (M–L, up
  to some size).
- Format → Convert: Base64, URL encoding, JSON string escapes, and SHA-256 of the selection or the file (S each).
- Several windows; drag a tab out into its own window (L).
- A hex view for binary files (M). Files with only CR line endings converted on open (S).
- Smooth scrolling (M). Right-to-left text (S–M). Any font and size (S).
- More code pages: Shift-JIS, GBK, Windows-125x, ISO-8859-x (M).
- Save as administrator through an elevated helper (M). An ARM64 build next to the x64 one (M; `Slate.exe` stays
  x64, since every installed Slate downloads that name).
- Drawing (what the 0.7.0 audit measured and left): with word wrap off, Page Down in a huge colored line takes
  ~70 ms, most of it laying out 32 new 8 KiB segments (smaller segments without wrap, or laying out only the columns
  in view) (M); Ctrl+End in a 30 MB code file takes ~250 ms on the UI thread, as the coloring index lexes the whole
  file (a faster states-only path in `code.rs`, or catching up over several frames with guessed colors) (S–M); the
  caret blink presents the whole window (FLIP_SEQUENTIAL with `Present1` dirty rects), up to three frames can queue
  while scrolling (`SetMaximumFrameLatency(1)`), and ClearType is used even when the user turned it off — each needs
  checking on a real screen (S each); while a big file's lines are counted, the line numbers' width is a guess, so
  the text moves once they're counted (S); emoji cost ~0.5 ms each to draw (a glyph cache) (M).
- The path bar and structure panel scan the JSON again once typing pauses after an edit (in the background, ~1.5 s
  for 800 MB): keep the lists through edits that don't change the structure, rescanning only the container edited
  (S–M); in a list kept sparse, one huge child is read again on every caret move inside it (keep a checkpoint after
  any child over 1 MiB) (S). Line tools on a selection run on the UI thread (sorting 16 MB takes ~0.2 s), and
  changing case is limited to 16 MB selections: run both like the whole document's line tools (S).
- The session: documents from a few MB up to 64 MiB are copied whole at least every 20 s while being edited (an
  hour on a 60 MB file writes ~10 GB): the pieces scheme above a few MB (S–M); at the start, copies are read on the UI
  thread and copied twice (S). Memory: per-tab layout and block caches kept while a tab is hidden, and undo keeping a
  Format's or Replace all's whole previous text in memory (trim, or spill to disk) (S–M). Typing after a snapshot
  starts a new piece (an append-only add store) (M).
- Engine: a piece tree with O(log n) lookups for documents with hundreds of thousands of edits (M); in files over
  64 MiB, regex matches longer than 1 MiB (Find next/previous) or 16 MiB (Count all, Replace all) that cross a
  search window: carry the regex's state across windows (M); UTF-16 files with unpaired surrogates kept byte-exact (S);
  saving where Slate can't create files, and keeping hard links (S–M); trimming what undo history keeps alive (S);
  Home/End on a big file whose lines are still being counted shouldn't read far (S); a big edited file whose time
  alone changed (touched, synced unchanged) can't be saved (only Reload): a whole-file hash taken while its lines are
  counted, if that costs the count nothing, would tell (S–M); the JSON outline stops at a container nested over
  50,000 deep: reading past it cost every scan 3-6%, tried twice (S).
- Tests: a table of the key bindings checked against the shortcut lists and the README (S); the close and quit
  prompts and the updater's swap in the smoke tests (S–M).
- Colors: a misdetected heredoc should end sooner than at its end word; exact states for lexers with look-ahead at
  the 8 KiB cuts of very long lines; VB ` _` continued comments, AutoHotkey continuation sections, R raw strings, C#
  holes inside nested interpolated strings, quotes inside Swift `\(…)` and Kotlin `${…}` holes, CSS
  `(min-width:768px)` without a space, C# members with capitals taken for types (S each). PHP, Lua, Ruby, Perl, R,
  Dart, Scala, PowerShell and shell scripts color at 15–25 cycles a byte: a fast path like the C family's (S each).
  XML panel rows could show their `[n]` (S).
- Window: the title's text dimmed while the window isn't active; the dark title bar on Windows 10 before 20H1; the
  structure panel by keyboard; tooltips for cut-short path bar parts and notices; dragging a tab scrolling a full
  tab strip (S each).
- Sort lines: `1.10` after `1.9` when the lines look like versions; `ß` as `ss` (S).
- Line tools keep mixed line endings instead of using the most common one (S). Indenting or commenting a selection
  whose last line ends in a lone CR treats it like the other lines (S).
- Updates: when closing is cancelled after an update was put in place, the next start should still be watched (and
  undone if it fails), and a setting a newer version wrote that this one can't read should survive this one saving
  (it matters when an update is undone) (S each).
- Restart after Windows restarts for an update, with the tabs back, as Notepad does (`RegisterApplicationRestart`)
  (S). A new version that hangs while starting should be undone like one that fails (S). A cap on `crash.log`'s size
  (S). A file handed to a Slate that answers too late (after 10 s) opens in both windows (S).
- Size: of the 3.4 MB, regex's Unicode tables are 380 KB. Without the age, boolean-property and break tables
  (`\p{Age=…}`, `\p{Alphabetic}`, `\p{Emoji}`, grapheme/word/sentence break classes) it's 155 KB smaller; without
  general categories and scripts too (`\p{L}`, `\p{Greek}`), 240 KB. Case-insensitive search in every language and
  `\w`, `\d`, `\s`, `\b` stay either way. To decide (S).

## Not planned

Plugins, project or workspace panels, language servers (IntelliSense), a terminal, Git: they'd make Slate slower to
start and heavier, and turn it into another IDE. Slate is the editor that opens anything instantly.

## Infrastructure

- Reproducible builds: a pinned toolchain, a build cache, no home paths or timestamps in the exe (S).
- CI: automated updater tests against a local stand-in for GitHub,
  and test-mode commands for the timers (S). (The smoke test writes a session and puts it back.)
- Test on what the development PC doesn't have: a FAT/exFAT drive, a Chinese/Japanese/Korean system locale, the
  "Use UTF-8 worldwide" option, a high-DPI second monitor and a taskbar on the side (S each).
- Check on a real screen what the window shows before its first frame (nothing is painted until the graphics device
  is there, a quarter of a second on a PC with a slow driver: see *Soon*), and whether switching the theme with the
  find bar open repaints its boxes (S).
- Install: the registry keys, ProgID and data folder are all named just "Slate": decide on final names before many
  people install it, since renaming later needs a migration.

## Known limits

These are trade-offs (mostly to keep huge files instant), not bugs:

- With word wrap off, a line longer than about 4 KiB still shows in rows of 8 KiB, and Home/End work per row.
- The scrollbar marks the first 1,000,000 matches of a search.
- In regular expressions a lone `\r` counts as a line break for `^` and `$`.
- A big document that Format, Replace All or a line-ending change rewrote isn't kept between runs yet (see *Next*),
  so closing asks about it.
- On keyboard layouts where Ctrl+Alt+S types a character (Polish, for one), Save All is in the File menu only.
