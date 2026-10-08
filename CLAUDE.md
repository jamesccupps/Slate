# Slate

A fast, simple Notepad replacement for Windows that opens huge files (800 MB+ JSON) instantly. Native Win32 +
Direct2D/DirectWrite, written in Rust. Single portable `Slate.exe`, nothing to install at runtime.

It is a general text editor first ("a better Notepad"); the JSON and XML extras (format/minify/check, the path bar and
the structure panel) only show for those files. It should be quick, easy to use, with only the features
people actually need, and as seamless as Steam/Claude-level apps. Keep this file and `docs/IDEAS.md` up to date as
work happens. The repo is on GitHub (`jamesccupps/Slate`): keep personal paths and machine settings out of
tracked files (the deploy folder lives in the git-ignored `.install-dir`). The user wants the repo kept current: push
finished work, and keep the README and the repo's description and topics up to date.

## Build

Rust with the **GNU** toolchain is enough (no Visual Studio needed). On the development PC it lives in
`%USERPROFILE%\.cargo` / `.rustup` and isn't on the PATH. Use `build.cmd` (sets PATH, builds release, copies
`Slate.exe` to `dist\`, and deploys to the folder in `.install-dir` if there is one) or:

```
set PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;%PATH%
cargo build --release
cargo test --lib
```

- The `windows` crate is pinned to **0.58**: newer versions link through raw-dylib, which needs `dlltool` + an
  assembler that the GNU toolchain doesn't bundle.
- `build.rs` writes the resource object (manifest, icon, version info) itself; there is no rc.exe/windres.
- `scripts/deploy.ps1 -Exe <exe> -Dir <folder> [-Portable]` copies a build into place, also while the old one runs
  (it renames the running exe out of the way; Slate deletes such leftovers on start).
- Never start Slate (or anything long-running) from a Claude session expecting it to persist: Claude is an MSIX app,
  so children run in its container (private HKCU/AppData, killed when Claude restarts). Tests render offscreen.

## Releasing

1. Bump `version` in `Cargo.toml` (the updater compares it with the release tag; CI refuses a tag that doesn't
   match). Write the notes in `docs/release-notes-<version>.md`; CI puts them in the draft.
2. Commit and push to `main`; then `git tag vX.Y.Z` and `git push origin vX.Y.Z`.
3. The Build workflow (`.github/workflows/build.yml`) tests, builds, runs the exe through `tests/smoke.txt` and
   drafts the release with `Slate.exe` and `Slate.exe.sha256`. The user publishes it — Slate's updater
   (`src/ui/update.rs`) only sees published releases.

What the updater reads from a release can never change, as every version out there reads it: the tag `vX.Y.Z` (no
pre-release suffix), assets named exactly `Slate.exe` and `Slate.exe.sha256` (`<64 hex digits>  Slate.exe`), this
repository, `/releases/latest`. Slate.exe stays the x64 build. The updater never goes back to a lower version, so a
bad release is fixed by publishing a higher one; a new version that doesn't start is undone on the user's PC by the
version before it (see update.rs). A copy deployed from a local build also updates itself from GitHub once a higher
version is published (unless its automatic check is off).

Commits use the GitHub no-reply address (repo-local git config); GitHub refuses pushes that would publish another one.

## Layout

- `src/core/` — the engine, no UI. Unit-tested (`cargo test --lib`).
  - `source.rs` — immutable byte sources: memory, or a file read on demand via a 64 KiB block cache (opened with
    full sharing, never locked). Newline index = cumulative count per 64 KiB block, built in the background; a file
    that only grew (a log) reuses the old index if sampled blocks (and the old last, incomplete block) still match.
    Reads never fail: missing bytes read as zeros and bump `read_errors` (save refuses to write then). Index reads
    are tried again; one that keeps failing stops the index (`index_error`: the doc stays pending), never a guessed
    count. Each file source keeps its file's stamp through its own handle (size, write/change times):
    `changed_in_place` tells another program writing into that file (the text read from it isn't the user's any
    more) from a file that only grew or was replaced by a new one (the handle keeps reading the old one). Bulk
    reads (index, hashing, big ranges for search and save, the stamp checks) go through a second handle on the same
    file (`ReOpenFile`), so the window's block reads don't queue behind them on a slow share. A source whose file
    isn't at its path any more (a save put a new one there, or the disk check found another file or none) is `gone`:
    a later run couldn't read it, so the session copies what's used of it.
  - `buffer.rs` — piece table in leaves of ≤256 pieces with cached byte/newline totals. Add buffer for typing;
    big inserts (≥1 MiB) get their own source. `snapshot()` freezes it for worker threads.
  - `document.rs` — undo/redo steps (typing/backspace runs coalesce; `seal()` ends a run), dirty state, change log
    for views, char/word navigation. "Pending" docs: big file whose index isn't done — readable, not editable.
    `version` is unique across all documents (caches key on it).
  - `io.rs` — open (≤64 MiB into memory; bigger = file-backed + background index; UTF-16/ANSI converted to UTF-8,
    big ones into a self-deleting temp file; ANSI only if the text converts back to exactly the file's bytes, else
    it stays UTF-8 with every byte kept; `open`/`reload` do all of it, canonical path and binary check included, on
    another thread) and save (temp file `.slate-save-*.tmp` in the same folder + POSIX-
    semantics rename with `\\?\` paths, so it works while we still read the old file; then MoveFileEx, then
    ReplaceFile for a file still open on a share/FAT drive — not atomic, so last, with a backup it restores; never
    writes the target in place). Save refuses text read from a file changed in place (`Changed`) and ANSI that
    would turn characters into "?" (`Lossy`, unless the user said so). Stale temp files of dead processes are
    cleaned up.
  - `search.rs` — byte-regex search in windows (8 MB, 64 KB overlap, grows for long matches), find next/prev,
    count all, streaming replace-all.
  - `json.rs` — one streaming tokenizer for pretty-print / minify / validate (JSON Lines OK), errors with offsets;
    comments (JSONC) are refused with a message saying so.
  - `xml.rs` — the same for XML (well-formedness check; markup separated only by whitespace with a line break goes
    on its own line, text is kept exactly, and nothing is added inside an element once it has text, so mixed content
    keeps its meaning; whitespace-only elements, spaces between elements on one line and `xml:space="preserve"` stay).
  - `lines.rs` — sort (natural: numbers with signs and decimals, case and accents ignored), remove duplicate / blank
    lines, trim line ends, change case.
  - `jsonnav.rs` — lazy JSON structure: the children of one container (lists over 100,000 keep every 64th child
    and rescan between them; lists of containers inside a scanned one already over 1024), the path at an offset
    (`data[1203].name`), previews. Comments are skipped; in a file cut short, open containers end at its end. A scan
    can also keep the lists of every container holding an offset (`path_to`): the path to the caret, however deep,
    then needs one scan, and a walk takes the last path's steps as they are where it goes the same way.
  - `xmlnav.rs` — the same for XML, filling in jsonnav's lists: the elements inside one element (names hashed as
    they're read, ~270 MB/s), the path (`catalog › book[3] › title`, the index among siblings of that name; XPath
    to copy), previews (attributes, and the text of an element without elements inside). Comments, CDATA, PIs and
    the DOCTYPE are skipped (in its subset, comments and PIs as they are: their quotes are just characters); an end
    tag closes the element it names (among the 1024 innermost), a stray one is ignored; in a file cut short, what's
    open ends at its end. An element scanned on its own stops where its parent's list says it ends, and a rescan
    where the list's content ends (`Children::content_end`), so malformed markup reads the same either way.
  - `text.rs` — encoding/EOL detection (mostly-UTF-8 with a few bad bytes stays UTF-8), UTF-16/ANSI codecs (ANSI
    = the system code page, Windows-1252 under the UTF-8 code page option), display decoding (control chars →
    symbols), char classes.
  - `job.rs` — background jobs with progress/cancel, notify the UI by posting a window message.
- `src/ui/` — the Win32 app (see the module docs at the top of each file).
  - `highlight.rs` (+ `highlight/code.rs`, `highlight/markup.rs`, `highlight/config.rs`) — syntax coloring for ~45
    languages: hand-written lexers that color one segment and return the `State` they end in (inside a block
    comment, a multi-line string, an XML tag, a Markdown code block...). `code.rs` is one configurable lexer for
    programming/scripting languages (keyword tables + per-language extras: Rust and C++ raw strings, PowerShell
    here-strings, Batch labels, heredocs, JS and Perl regexes, Perl's `qw(…)`/`s{…}{…}` and POD, C#/HCL strings in
    interpolation holes, AutoHotkey hotkeys, VB's `Rem`...); `config.rs` the line-based formats (TOML, nginx,
    Apache, `.properties`, SRT/WebVTT, iCalendar/vCard, `.sln`); PHP files are HTML with PHP inside. A Markdown
    ``` block is colored as the language it names, that lexer's state kept in the Markdown state (`mode` holds the
    language; where its state doesn't fit, each line is colored from its line start). Where a quote is easily a
    stray one (shell, SQL, PHP...), a string left open gives up at a blank line or after 40 lines. Language is
    picked by file name (templates like `x.yaml.j2` by the name inside; nginx/Apache configuration also by its
    folder, so detection gets the whole path), then by content (`#!` lines, `<?xml`, `server {`, `<VirtualHost`,
    `WEBVTT`, JSON that reads as JSON, log timestamps — not IP addresses). A test checks that lexing a text in two
    pieces ends in the same state as lexing it whole (cut after a line break for every language, mid-line too for
    those listed in it); for deeper runs raise its counts for a while.
  - `app.rs` state, layout and painting; `actions.rs` input, commands, background jobs, saving and closing;
    `editor.rs` the text view; `structure.rs` path bar + structure panel; `findbar.rs`; `session.rs` (tabs and
    unsaved text kept between runs); `settings.rs` (data folder, portable mode); `install.rs` ("Open with" entries
    in HKCU); `update.rs` (updates from GitHub releases: WinHTTP, SHA-256 check, swap the exe, restart with
    `--updated`, undone if the new one doesn't start); `crash.rs` (a minidump next to crash.log on a native crash,
    written by a thread waiting for it from the start; Help → Report a problem opens a filled-in GitHub issue form
    the user sends themselves; the version string with the commit, which build.rs passes in); `testmode.rs`.
  - `gfx.rs` — Direct2D drawing into a D3D11 **flip-model swap chain** (FLIP_DISCARD, then FLIP_SEQUENTIAL), with
    `ID2D1HwndRenderTarget` only as a fallback: on a PC with a Parsec virtual display adapter the HWND target reported
    "occluded" and drew nothing. Device loss → `discard_target()` and paint again.

## Design decisions

- Internal text is UTF-8 bytes; files in other encodings are converted on open and back on save (BOM kept as found).
- Lines are split only at `\n` (`\r\n` handled as one). Lone `\r` shows as a symbol.
- Display "segments": a line longer than ~4 KiB is cut at fixed 8 KiB grid points (only where no line break is
  within 4 KiB before), so a 800 MB one-line JSON renders and scrolls instantly. Segments never need the index.
  Without word wrap a row is a whole segment, mostly off to the side: per-character work on a row (whitespace marks)
  looks only at the part in view, with x positions from the layout's cluster metrics (a DirectWrite hit test costs
  more the further into a long layout it is).
- Scrollbar is proportional to bytes, not lines, so it needs no layout of the whole file.
- Coloring across lines: documents up to 32 MiB keep lexer states at checkpoints (every ~16 KiB, at line starts) in
  `editor::HlIndex`, plus the start state of segments already seen (always worked out from the checkpoint before,
  never from another segment start); an edit drops only what comes after it (the
  index applies the document's pending changes itself, as layout can run before `View::sync`). Measuring the scroll
  limit uses guessed states, so it never reads the whole file. Bigger files color each line on its own. Layouts are
  cached by (bytes, state).
- A general editor first: JSON and XML extras (format/minify/check, the path bar and structure panel) only appear in
  menus for those files; everything else gets the general tools (line tools, toggle comment, change case).
- Indentation is per document (`Tab::indent`): detected from the text (tabs, or spaces and their step), always tabs
  for TSV files and makefiles, else the settings' default; Format → Indentation sets the current document's (and
  the default). Line operations over many lines (indent, comment) rewrite the lines as one replacement.
- Keys: Ctrl+Alt shortcuts don't fire when the key types a character (AltGr is Ctrl+Alt: Polish AltGr+S is "ś");
  Alt on its own gives the custom menu bar the keyboard (WM_SYSCOMMAND SC_KEYMENU); a key that opens a menu drops
  the character it queued. Caret movement and Delete go by visible characters (`text::cluster_len_at`). Insert
  toggles overtype (not kept: "OVR" in the status bar), which never types over a line break.
- Accessibility: while Windows' high contrast is on, the theme is built from `GetSysColor` (`Theme::high_contrast`):
  everything on the window color with borders, no syntax colors, selected text drawn again in the highlight's text
  color, no light/dark switch; it's read again on WM_SETTINGCHANGE (SPI_SETHIGHCONTRAST), WM_SYSCOLORCHANGE and
  WM_THEMECHANGED. A hidden system caret (`win::follow_caret`, never shown) is moved to the drawn caret after each
  paint while the text has the keyboard, with EVENT_OBJECT_LOCATIONCHANGE for OBJID_CARET; WM_KILLFOCUS destroys it
  (the find box's edit control makes its own), and so does the caret scrolling out of view. UI Automation is designed in the roadmap, not built.
- Status bar counts: a selection up to 4 MiB is counted at once; a document up to 1 MiB at once, up to 64 MiB on
  another thread (after an edit only once the typing pauses: the count's snapshot ends the piece typing goes into),
  a bigger one not at all. Words are runs of non-space characters, like `wc -w`.
- `Tab::goto` is where a tab goes once its document is ready and it's the tab shown: `slate file.txt:120:5` (only
  when the path as given doesn't exist; a name with any other colon never becomes a new file, as it would be an
  alternate data stream) and Reopen closed tab (the last 20 tabs closed that had a file, back at their place).
- The App lives in `Rc<RefCell<App>>`. Anything that shows a dialog or menu runs from the `Deferred` queue outside
  the borrow. Dialogs run a modal loop in which timers and job messages still arrive (tabs can close, open or move),
  so code that shows one finds its tab again **by id** afterwards, never by an index taken before. The queue isn't
  drained again from inside such a loop: what was queued meanwhile (an Exit, another prompt) waits until it's closed.
- Never lose work: closing the window keeps unsaved documents in the session (like Windows 11 Notepad); only those
  it can't keep are asked about (saying why): big ones that would have to copy over 64 MiB from files (Format,
  Replace all or a conversion of a file over 64 MiB; a file replaced while the text still reads the old one), or all
  if the session can't be written; with the session turned off, also big tabs from last time that aren't back yet.
  Reload is refused while the tab is being saved (in a box, before anything else). A tab closed while it is saving
  closes once the save is done — unless the text changed meanwhile. On shutdown, unsaved work that can't be kept
  blocks it with a reason (`ShutdownBlockReasonCreate`), so Windows asks the user. Saving in ANSI never turns
  characters into "?" without asking first (Save as UTF-8 / ANSI anyway / Cancel), and a tab or window never closes
  after such a save.
- Files changed by other programs are looked for every 2 s and when the window is activated, on a background thread
  (`check_disk`, then `poll_disk`; a network drive that went away mustn't freeze the window). A clean document
  reloads (one whose lines are still being read, once they are); with unsaved changes the user is told, and for a
  big file another program wrote into, "Keep mine" isn't offered: saving would mix the two versions, so it's refused.
- Opening and reloading never touch the file system on the UI thread: the tab is there at once and its file is read
  on another thread (`Tab::load_job`); opening waits up to 150 ms for that (`OPEN_WAIT`), so only a slower file
  shows "Opening…". "Open already?" goes by name, then by the canonical path found when a file was read or saved.
  A tab from the session keeps its entry as read (`Tab::place`) until its file is read.
- The session (session.rs) holds that unsaved text, so: files are flushed to disk before they replace the old ones;
  reading is lenient (one bad tab or a value from a newer version loses nothing else; settings too); Slate deletes
  only backups it wrote or read itself, and backups no tab refers to come back as new tabs; restoring that crashes is
  caught and the session set aside. While editing it's written on another thread; closing writes it in place.
  Documents up to 64 MiB are backed up as a copy. Bigger ones as their pieces: `<name>.pieces` (the piece list, with
  the `Identity` of each file of the user's it reads: stamp, file id, sample hashes; rewritten when the text
  changed, or when a file it names is `gone` or not read any more: after a save, or another file found in its place)
  and `<name>.data` (what a later run couldn't read otherwise: typed and pasted text, each source
  once, and the parts used of a self-deleting temp file or a `gone` file, up to 64 MiB; appended with a hash per
  write, flushed before the list that refers to it replaces the old one; written anew under another name with only
  what's used once most of it isn't, as `<name>.data.tmp` until the list naming it is there: at the start,
  `finish_rewrites` keeps whichever has the newest text whole when a crash cut that short). After saving a big file
  the text reads the saved file, also if it changed during the save (`Buffer::move_onto`). A write that fails waits
  longer each time, and the session counts as not all written until it's done, so the timer tries again; a disk
  without room for it (and an eighth more, at least 1 MiB) isn't written to. Putting one back runs on another thread
  behind a tab that
  waits (`session::Restoring`): only if each file is the same one, not shorter, and unchanged or only grown (a log),
  with the same sample hashes; otherwise the added text comes back on its own in a new tab, read from `<name>.data`
  where it is (and the files go into `damaged\`, kept 30 days), never laid over something else. A tab whose file
  doesn't answer (a share that's gone, a file another program holds, a drive or folder that isn't there: only "not
  found" in a folder that is means gone) waits too and looks again, less often each time, as long as it's one of
  those; until a tab is back, the session keeps its entry as read.
- One Slate per user session: a second start hands its files to the running one. A Slate running as administrator
  is separate (its own lock, window class and session), as Windows doesn't let the two talk.
- Updates: the version being replaced starts the new one with `--updated` and waits ~15 s; if it can't start or ends
  with an error, it puts itself back and starts again with `--update-failed <version>` (that version isn't offered
  by the daily check again). Old copies are deleted by the next normal start. 0.2.0 starts the new one with
  `--wait-for <pid>` instead (still understood).
- Panics in message handling are caught, logged to `crash.log` in the data folder, the session is saved and a
  message shown. A panic while painting is only logged (showing a message would paint, and fail, again). A panic in
  a background job comes back as that job's failure (`job::Failure`), never as a job that runs forever.
- The keyboard focus is read with `GetFocus()` when painting, not tracked from WM_SETFOCUS (which can't reach the
  App while it is borrowed).

## Testing

- `cargo test --lib` — core tests (~4 s).
- `Slate.exe --test script.txt` drives the real app in a hidden window and renders frames offscreen into PNGs; the
  commands are listed at the top of `src/ui/testmode.rs` (`open:`, `type:`, `key:`, `cmd:`, `jobs`, `shot:`,
  `print:`, `expect:`, `answer:`, `lang:<name>` …; `print:menu0`…`menu4` lists a menu's items). In this mode
  prompts are never shown (they take answers from `answer:` lines and are listed by `print:asked`), nor are menus
  (`print:opened` lists them), the clipboard is a private one, and settings/session aren't written (unless
  `persist`, which needs `SLATE_DATA_DIR`). Mouse drags, raw wheel deltas, Alt, AltGr and window activation have
  their own commands (`down:`/`move:`/`up:`, `wheelraw:`, `altkey`, `altgr:on`, `activate`…), and `contrast:on`
  pretends Windows' high contrast is on (`print:theme`, `print:syscaret`, `print:statusbar` check the results).
  `SLATE_DATA_DIR` sets the data folder, `SLATE_TEST_LOG` the log file; `session:save|soon|restore` writes the
  session now, writes it the way the timer does, or restores it. `SLATE_UPDATE_TEST_VERSION=0.1.0` makes Slate
  believe it's that version (to try the updater against the real latest release, in a scratch folder);
  `SLATE_TEST_SLOW_OPEN=<ms>` makes reading each file take that much longer (a slow network drive).
- `tests/smoke.txt` is the smoke test CI runs on the built exe (paths in it are relative to the working folder).
- `SLATE_TEST_VISIBLE=1` runs the same scripts in the real window (on top, without taking the focus), drawing
  through the real swap chain; `shot:` then captures the screen.
- Don't drive the user's desktop with real mouse/keyboard input.
- Keep small changes small: a few checks for the risky part (see the user's preference in memory).
