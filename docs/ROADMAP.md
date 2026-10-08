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

## Next

1. **Big unsaved files survive closing (M–L).** Documents over 64 MiB are edited on top of the original file and
   aren't kept in the session, so closing asks about them. Keep their edits (the piece list, typed text and the
   original file's identity) in the session too, and copy the original aside if another program starts rewriting it.
2. **Network shares (M).** Restore a tab whose share doesn't answer as a placeholder that retries, instead of
   leaving it out; read files up to 64 MiB and reloads on a background thread (today they're read while the window
   waits); give background readers their own file handle so a slow share doesn't make the window wait.
3. **Accessibility (S, then L).** A high-contrast theme built from Windows' colors; a system caret so Magnifier and
   other tools can follow the cursor; then UI Automation, so screen readers can read the text.
4. **Release basics (S–M).** A license (the owner's choice: MIT or Apache-2.0 are the usual ones for an app like
   this); code signing (SignPath Foundation is free for open source; Azure Artifact Signing is about $10 a month) so
   Windows stops warning, then the updater also checks the signature; winget and Scoop manifests.
5. **Crash reports without telemetry (S–M).** A minidump on a native crash, and *Help → Report a problem* that
   opens a prefilled GitHub issue the user reviews and sends themselves. The commit hash in the version info and
   crash.log.
6. **Languages people open in a Notepad replacement (S each).** VBScript/VBA, AutoHotkey, real TOML, nginx and
   Apache configuration; then Perl, R, Terraform/HCL, CMake, `.properties`, subtitles (`.srt`/`.vtt`), calendars and
   contacts (`.ics`/`.vcf`), `.sln`. Better detection of `.env.*`, `CMakeLists.txt`, `Jenkinsfile`, `.htaccess`,
   `.npmrc`, `.j2` and the `hosts` file.

## Soon

- **Print** and page setup (M).
- **Find:** in the selection, a history, `\n` and `\t` in regex replacements, find in all open tabs (S–M).
- **Editing:** show spaces and line endings, matching brackets, column (Alt+drag) selection, reopen a closed tab,
  close all / close saved tabs, overtype (S each). Drag and drop of text (M).
- **Spell check** like Notepad's (M). **Inline IME composition** for Chinese, Japanese and Korean (M).
- **Tabs in the title bar**, like Windows 11 Notepad, to save space (M). A list of tabs when there are too many.
- **Status bar:** character and word counts, the indentation in use (S).
- **Settings in the menus** that only exist in settings.json today: restore last session, JSON indent (S).
- **XML** path bar and structure panel, like JSON's (M). Colors for Markdown code blocks by their language (S).
- **Open at a line:** `slate file.txt:120`, and a taskbar jump list (S).
- **JSON structure panel:** keep an open array element open when elements are inserted before it (it's keyed by
  index today); while the panel is updating after an edit, clicks go to the row's old place (S).

## Later

- Several windows; drag a tab out into its own window (L).
- A hex view for binary files (M). Files with only CR line endings converted on open (S).
- Smooth scrolling (M). Right-to-left text (S–M). Any font and size (S).
- More code pages: Shift-JIS, GBK, Windows-125x, ISO-8859-x (M).
- Save as administrator through an elevated helper (M). An ARM64 build next to the x64 one (M; `Slate.exe` stays
  x64, since every installed Slate downloads that name).
- Engine: a piece tree with O(log n) lookups for documents with hundreds of thousands of edits (M); regex matches
  longer than 64 KiB that cross a search window (M); UTF-16 files with unpaired surrogates kept byte-exact (S);
  saving where Slate can't create files, and keeping hard links (S–M); trimming what undo history keeps alive (S);
  Home/End on a big file whose lines are still being counted shouldn't read far (S).
- Colors: the rest of the small gaps (TOML `"""` strings, indented `.sass` properties, YAML quoted strings over
  several lines, C++ `1'000'000` and raw strings, C# nested interpolation); a misdetected heredoc should end sooner
  than at its end word; exact states for lexers with look-ahead at the 8 KiB cuts of very long lines (S each).
- Sort lines: `1.10` after `1.9` when the lines look like versions; `ß` as `ss` (S).
- Line tools keep mixed line endings instead of using the most common one (S).

## Infrastructure

- Reproducible builds: a pinned toolchain, `--locked`, a build cache, no home paths or timestamps in the exe (S).
- CI: write permission only for the release step (S); automated updater tests against a local stand-in for GitHub,
  and test-mode commands for the timers (S).
- Test on what the development PC doesn't have: a FAT/exFAT drive, a Chinese/Japanese/Korean system locale, the
  "Use UTF-8 worldwide" option, a high-DPI second monitor and a taskbar on the side (S each).
- Check whether the window shows a white frame before its first paint, and whether switching the theme with the find
  bar open repaints its boxes (S).
- Install: keep the version shown in *Installed apps* current after updates, a quiet uninstall entry, `Win+R slate`
  (S). The registry keys, ProgID and data folder are all named just "Slate": decide on final names before many
  people install it, since renaming later needs a migration.

## Known limits

These are trade-offs (mostly to keep huge files instant), not bugs:

- With word wrap off, a line longer than about 4 KiB still shows in rows of 8 KiB, and Home/End work per row.
- The scrollbar marks the first 1,000,000 matches of a search.
- In regular expressions a lone `\r` counts as a line break for `^` and `$`.
- Unsaved changes to a document over 64 MiB aren't kept between runs yet (see *Next*), so closing asks about them.
- On keyboard layouts where Ctrl+Alt+S types a character (Polish, for one), Save All is in the File menu only.
