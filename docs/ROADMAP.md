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

## Next

1. **Accessibility: UI Automation (L).** The high-contrast theme (Windows' colors, read again when they change) and a
   hidden system caret that Magnifier and other tools follow are done. Next, so screen readers can read the text, a
   provider for the text area:
   - `WM_GETOBJECT` (`UiaRootObjectId`) answers `UiaReturnRawElementProvider` with one COM object:
     `IRawElementProviderSimple` (Document control type, the tab's title as its name, `ServerSideProvider |
     UseComThreading` so calls come on the UI thread) and `ITextProvider` (`DocumentRange`, `GetSelection`,
     `GetVisibleRanges`, `RangeFromPoint`). The tab strip, menus and status bar can wait; the find bar's boxes are
     native and already readable.
   - `ITextRangeProvider` as two byte offsets into the active document, kept up to date through its change log like
     the scroll position. Units: character (as the caret moves), word (`word_left`/`word_right`), line (the view's
     rows), paragraph (lines), document. `GetText` reads at most what it's asked for (a range can be a whole 800 MB
     file); `GetBoundingRectangles` only for rows on screen; `Select`, `ScrollIntoView`, `Compare`/`Move` endpoints.
   - Events: text selection changed after the caret moves, text changed after edits.
   - Why it's L: COM classes through the windows crate's `implement` feature; the provider is called while the App
     may be borrowed (`try_borrow`, else `UIA_E_ELEMENTNOTAVAILABLE`, and never a panic across COM); offsets to
     UTF-16 over huge documents and ones whose lines are still being counted; testing with Narrator and NVDA.
2. **Release basics (S–M).** Code signing (SignPath Foundation is free for open source; Azure Artifact Signing is
   about $10 a month) so Windows stops warning, then the updater also checks the signature; winget and Scoop
   manifests. (Licensed MIT since October 2026.)
3. **Big unsaved files, the rest (S–M).** Compact a big document's `.data` while it stays unsaved (it only grows
   now), keep its newline index in the session so putting it back doesn't count the lines again, and keep big
   documents that Format or Replace All rewrote (today closing still asks about those).

## Soon

- **Print** and page setup (M).
- **Find:** in the selection, a history, `\n` and `\t` in regex replacements, find in all open tabs (S–M).
- **Editing:** column (Alt+drag) selection (S). Drag and drop of text (M).
- **Spell check** like Notepad's (M). **Inline IME composition** for Chinese, Japanese and Korean (M).
- **Tabs in the title bar**, like Windows 11 Notepad, to save space (M).
- **A taskbar jump list** with the recent files (S).
- **JSON structure panel:** keep an open array element open when elements are inserted before it (it's keyed by
  index today); while the panel is updating after an edit, clicks go to the row's old place (S).

## Later

- **A Linux version, Raspberry Pi included (L).** The engine (`src/core`: text storage, search, JSON/XML, line
  tools) is plain Rust except for a few file-handling and ANSI code-page calls, which go behind a small platform
  layer. The window and drawing get a second front end on a cross-platform toolkit (GTK 4, or winit with a 2D GPU
  renderer and a text-shaping crate; to be decided by what keeps huge files instant). Builds for x86-64 and ARM64
  (the Pi) in CI, packaged as an AppImage or .deb; release files get their own names, so the Windows updater's
  `Slate.exe` / `Slate.exe.sha256` stay exactly as they are.
- **Maybe: a Markdown preview** (a rendered view next to the text, off by default) (M). Only if people ask for
  it: Slate is meant to stay quick and open anything, so it must cost nothing while it isn't used.
- Several windows; drag a tab out into its own window (L).
- A hex view for binary files (M). Files with only CR line endings converted on open (S).
- Smooth scrolling (M). Right-to-left text (S–M). Any font and size (S).
- More code pages: Shift-JIS, GBK, Windows-125x, ISO-8859-x (M).
- Save as administrator through an elevated helper (M). An ARM64 build next to the x64 one (M; `Slate.exe` stays
  x64, since every installed Slate downloads that name).
- Drawing: a long line packed with colors (minified XML, ~2,700 tags per 8 KiB) costs 70–130 ms per caret move, against
  ~17 ms as plain text: lay out, color and hit-test only the part of a segment that's in view (M).
- Engine: a piece tree with O(log n) lookups for documents with hundreds of thousands of edits (M); regex matches
  longer than 64 KiB that cross a search window (M); UTF-16 files with unpaired surrogates kept byte-exact (S);
  saving where Slate can't create files, and keeping hard links (S–M); trimming what undo history keeps alive (S);
  Home/End on a big file whose lines are still being counted shouldn't read far (S).
- Colors: a misdetected heredoc should end sooner than at its end word; exact states for lexers with look-ahead at
  the 8 KiB cuts of very long lines; VB ` _` continued comments, AutoHotkey continuation sections, R raw strings, C#
  holes inside nested interpolated strings (S each). XML panel rows could show their `[n]` (S).
- Sort lines: `1.10` after `1.9` when the lines look like versions; `ß` as `ss` (S).
- Line tools keep mixed line endings instead of using the most common one (S). Indenting or commenting a selection
  whose last line ends in a lone CR treats it like the other lines (S).
- Updates: when closing is cancelled after an update was put in place, the next start should still be watched (and
  undone if it fails), and a setting a newer version wrote that this one can't read should survive this one saving
  (it matters when an update is undone) (S each).

## Infrastructure

- Reproducible builds: a pinned toolchain, a build cache, no home paths or timestamps in the exe (S).
- CI: write permission only for the release step (S); automated updater tests against a local stand-in for GitHub,
  and test-mode commands for the timers (S).
- Test on what the development PC doesn't have: a FAT/exFAT drive, a Chinese/Japanese/Korean system locale, the
  "Use UTF-8 worldwide" option, a high-DPI second monitor and a taskbar on the side (S each).
- Check whether the window shows a white frame before its first paint, and whether switching the theme with the find
  bar open repaints its boxes (S).
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
