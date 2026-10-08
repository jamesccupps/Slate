# Slate

A fast, simple Notepad replacement for Windows that opens huge files (800 MB+ JSON) instantly. Native Win32 +
Direct2D/DirectWrite, written in Rust. Single portable `Slate.exe`, nothing to install at runtime.

It is a general text editor first ("a better Notepad"); the JSON and XML extras (format/minify/check, and for JSON the
path bar and structure panel) only show for those files. It should be quick, easy to use, with only the features
people actually need, and as seamless as Steam/Claude-level apps. Keep this file and `docs/IDEAS.md` up to date as
work happens. The repo is on GitHub (`jamesccupps/Slate`): keep personal paths and machine settings out of
tracked files (the deploy folder lives in the git-ignored `.install-dir`), and push only when the user says so.

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

1. Bump `version` in `Cargo.toml` (the updater compares it with the release tag).
2. Commit and push to `main`; then `git tag vX.Y.Z` and `git push origin vX.Y.Z`.
3. The Build workflow (`.github/workflows/build.yml`) tests, builds and drafts the release with `Slate.exe` and
   `Slate.exe.sha256`. The user edits the notes and publishes it — Slate's updater (`src/ui/update.rs`) only sees
   published releases.

Commits use the GitHub no-reply address (repo-local git config); GitHub refuses pushes that would publish another one.

## Layout

- `src/core/` — the engine, no UI. Unit-tested (`cargo test --lib`).
  - `source.rs` — immutable byte sources: memory, or a file read on demand via a 64 KiB block cache (opened with
    full sharing, never locked). Newline index = cumulative count per 64 KiB block, built in the background; a file
    that only grew (a log) reuses the old index if sampled blocks still match. Reads never fail: missing bytes read
    as zeros and bump `read_errors` (save refuses to write then).
  - `buffer.rs` — piece table in leaves of ≤256 pieces with cached byte/newline totals. Add buffer for typing;
    big inserts (≥1 MiB) get their own source. `snapshot()` freezes it for worker threads.
  - `document.rs` — undo/redo steps (typing/backspace runs coalesce; `seal()` ends a run), dirty state, change log
    for views, char/word navigation. "Pending" docs: big file whose index isn't done — readable, not editable.
    `version` is unique across all documents (caches key on it).
  - `io.rs` — open (≤64 MiB into memory; bigger = file-backed + background index; UTF-16/ANSI converted to UTF-8,
    big ones into a self-deleting temp file) and save (temp file `.slate-save-*.tmp` in the same folder + POSIX-
    semantics rename with `\\?\` paths, so it works while we still read the old file; MoveFileEx fallback; never
    writes the target in place). Stale temp files of dead processes are cleaned up.
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
    (`data[1203].name`), previews. Comments are skipped; in a file cut short, open containers end at its end.
  - `text.rs` — encoding/EOL detection, UTF-16/ANSI codecs, display decoding (control chars → symbols), char classes.
  - `job.rs` — background jobs with progress/cancel, notify the UI by posting a window message.
- `src/ui/` — the Win32 app (see the module docs at the top of each file).
  - `highlight.rs` (+ `highlight/code.rs`, `highlight/markup.rs`) — syntax coloring for ~30 languages: hand-written
    lexers that color one segment and return the `State` they end in (inside a block comment, a multi-line string, an
    XML tag, a Markdown code block...). `code.rs` is one configurable lexer for programming/scripting languages
    (keyword tables + per-language extras: Rust raw strings, PowerShell here-strings, Batch labels, heredocs, JS
    regexes...); PHP files are HTML with PHP inside. Where a quote is easily a stray one (shell, SQL, PHP...), a
    string left open gives up at a blank line or after 40 lines. Language is picked by file name, then by content
    (`#!` lines, `<?xml`, JSON that reads as JSON, log timestamps). A test checks that lexing a text in two pieces
    ends in the same state as lexing it whole.
  - `app.rs` state, layout and painting; `actions.rs` input, commands, background jobs, saving and closing;
    `editor.rs` the text view; `structure.rs` path bar + structure panel; `findbar.rs`; `session.rs` (tabs and
    unsaved text kept between runs); `settings.rs` (data folder, portable mode); `install.rs` ("Open with" entries
    in HKCU); `update.rs` (updates from GitHub releases: WinHTTP, SHA-256 check, swap the exe, restart with
    `--wait-for <pid>`); `testmode.rs`.
  - `gfx.rs` — Direct2D drawing into a D3D11 **flip-model swap chain** (FLIP_DISCARD, then FLIP_SEQUENTIAL), with
    `ID2D1HwndRenderTarget` only as a fallback: on a PC with a Parsec virtual display adapter the HWND target reported
    "occluded" and drew nothing. Device loss → `discard_target()` and paint again.

## Design decisions

- Internal text is UTF-8 bytes; files in other encodings are converted on open and back on save (BOM kept as found).
- Lines are split only at `\n` (`\r\n` handled as one). Lone `\r` shows as a symbol.
- Display "segments": a line longer than ~4 KiB is cut at fixed 8 KiB grid points (only where no line break is
  within 4 KiB before), so a 800 MB one-line JSON renders and scrolls instantly. Segments never need the index.
- Scrollbar is proportional to bytes, not lines, so it needs no layout of the whole file.
- Coloring across lines: documents up to 32 MiB keep lexer states at checkpoints (every ~16 KiB, at line starts) in
  `editor::HlIndex`, plus the start state of segments already seen (always worked out from the checkpoint before,
  never from another segment start); an edit drops only what comes after it (the
  index applies the document's pending changes itself, as layout can run before `View::sync`). Measuring the scroll
  limit uses guessed states, so it never reads the whole file. Bigger files color each line on its own. Layouts are
  cached by (bytes, state).
- A general editor first: JSON and XML extras (format/minify/check, the path bar and structure panel) only appear in
  menus for those files; everything else gets the general tools (line tools, toggle comment, change case).
- The App lives in `Rc<RefCell<App>>`. Anything that shows a dialog or menu runs from the `Deferred` queue outside
  the borrow. Dialogs run a modal loop in which timers and job messages still arrive (tabs can close, open or move),
  so code that shows one finds its tab again **by id** afterwards, never by an index taken before.
- Never lose work: closing the window keeps unsaved documents ≤64 MiB in the session (like Windows 11 Notepad);
  only bigger ones (or all, if the session can't be written) are asked about. A tab closed while it is saving closes
  once the save is done — unless the text changed meanwhile. On shutdown, unsaved work that can't be kept blocks it
  with a reason (`ShutdownBlockReasonCreate`), so Windows asks the user.
- Panics in message handling are caught, logged to `crash.log` in the data folder, the session is saved and a
  message shown. A panic while painting is only logged (showing a message would paint, and fail, again).
- The keyboard focus is read with `GetFocus()` when painting, not tracked from WM_SETFOCUS (which can't reach the
  App while it is borrowed).

## Testing

- `cargo test --lib` — core tests (~4 s).
- `Slate.exe --test script.txt` drives the real app in a hidden window and renders frames offscreen into PNGs; the
  commands are listed at the top of `src/ui/testmode.rs` (`open:`, `type:`, `key:`, `cmd:`, `jobs`, `shot:`,
  `print:`, `expect:`, `answer:`, `lang:<name>` …; `print:menu0`…`menu4` lists a menu's items). In this mode
  prompts are never shown (they take answers from `answer:` lines and are listed by `print:asked`), the clipboard
  is a private one, and settings/session aren't written (unless `persist`). `SLATE_DATA_DIR` sets the data folder,
  `SLATE_TEST_LOG` the log file.
- `SLATE_TEST_VISIBLE=1` runs the same scripts in the real window (on top, without taking the focus), drawing
  through the real swap chain; `shot:` then captures the screen.
- Don't drive the user's desktop with real mouse/keyboard input.
- Keep small changes small: a few checks for the risky part (see the user's preference in memory).
