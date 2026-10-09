# Slate

A fast, simple Notepad replacement for Windows that opens huge files (800 MB+ JSON) instantly. Native Win32 +
Direct2D/DirectWrite, written in Rust. Single portable `Slate.exe`, nothing to install at runtime. Since 0.8.0 also on
Linux (x86-64 and ARM64, the user's Raspberry Pi 5): the same engine, colors and editing in a GTK 4 window
(`src/gtk`), packaged as a `.deb`.

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

Linux: GTK 4.8 or newer (`gtk4` crate 0.11 with `v4_8`, as Debian 12 and Raspberry Pi OS 12 have it) and `libc`;
`cargo build --release` gives `target/release/Slate` (installed as `slate`). On the Windows development PC it's built
and tested in Docker (Docker Desktop): an image from `rust:1-bookworm` with `libgtk-4-dev xvfb xauth
fonts-dejavu-core fonts-noto-color-emoji dbus-x11`, the source copied into a volume first (a `tar` of the folder
piped into `docker run -i -v slate-src:/src`: building from the Windows folder through WSL is slow and makes WSL show
the user "Performance Tip" notifications), `CARGO_TARGET_DIR` in a volume, and the
program run under `xvfb-run` with `GTK_A11Y=none GSK_RENDERER=cairo` (`--test` scripts; for the real window's
dialogs and menus, `Xvfb` + `dbus-run-session` + `xdotool` and ImageMagick's `import` for pictures, as test mode
never shows a dialog). `cargo check --target x86_64-unknown-linux-gnu` on Windows doesn't work any more (GTK's
crates need the Linux pkg-config). `packaging/linux/package.sh <binary>
<version> <amd64|arm64> <out>` makes the `.deb` (`/usr/bin/slate`, the desktop file `io.github.jamesccupps.Slate`,
icons) and a tarball, each with a `.sha256`.

## Releasing

1. Bump `version` in `Cargo.toml` (the updater compares it with the release tag; CI refuses a tag that doesn't
   match). Write the notes in `docs/release-notes-<version>.md`; CI puts them in the draft.
2. Commit and push to `main`; then `git tag vX.Y.Z` and `git push origin vX.Y.Z`.
3. The Build workflow (`.github/workflows/build.yml`) tests, builds, runs the exe through `tests/smoke.txt`; the
   Linux jobs (x86-64 and ARM64, in a `debian:12` container so the result runs on Debian 12 / Raspberry Pi OS 12
   and newer) test, build, run `tests/smoke-linux.txt` under Xvfb and package (a build that isn't from a tag as
   `<version>~dev<run>`, just below the release, so a test package installs over the last release and the release
   over it); then a release job (the only one
   with write access) drafts the release with `Slate.exe`, `Slate.exe.sha256` and the `.deb`s and tarballs (with
   their `.sha256`). The user publishes it — Slate's updater (`src/ui/update.rs`) only sees published releases.
   Before the draft, the `sign` job signs `Slate.exe` in Azure Artifact Signing (account `SlateAccount`, certificate
   profile `Slate`), checks the signature, runs the smoke test on the signed exe and hashes it again.
   It signs in to Azure with GitHub's OIDC token (no password anywhere): the app registration "Slate signing"
   trusts only `repo:jamesccupps@148652101/Slate@1409482725:environment:release` (GitHub's immutable subject:
   owner and repository IDs, the repo's default), and the `release` environment only `v*` tags; the
   repository secrets `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` say which app.
4. Publishing the release runs `.github/workflows/apt.yml` (named "apt repository"; it also publishes the website, and
   runs again when `site/` or `docs/images/` change on `main`) (it starts itself again on `main`, as GitHub Pages only
   publishes from there): `packaging/linux/apt-repo.sh` makes the apt repository from the latest release's `.deb`s,
   signed with the secret `APT_SIGNING_KEY`, and Pages (Source: GitHub Actions) publishes it at
   `https://jamesccupps.github.io/Slate/apt` (suite `stable`, component `main`). Every `.deb` brings the public key
   (`packaging/linux/slate-archive-keyring.gpg` → `/usr/share/keyrings/`) and adds
   `/etc/apt/sources.list.d/slate.list` when installed (removed with the package), so `apt upgrade` updates Slate.
   The private key's only other copy is on the development PC in `%USERPROFILE%\.slate-apt-key` (never in the
   repo). The repository's URL, suite, component, keyring path and key can never change, as every installed Slate
   reads them: with another key, apt refuses the repository until a `.deb` bringing the new key is installed by
   hand.

5. winget: the package is `JamesCupps.Slate` in microsoft/winget-pkgs (first submitted with 0.8.1:
   microsoft/winget-pkgs#449740, from the user's fork `jamesccupps/winget-pkgs`). Its installer is the release's
   `Slate.exe`, `InstallerType: exe`, `Scope: user`, silent switch `--install`, `ProductCode: Slate` (the Installed
   apps key), `UpgradeBehavior: install`. Each new version needs its own manifest PR (three files under
   `manifests/j/JamesCupps/Slate/<version>/`, with the new `InstallerUrl` and `InstallerSha256`; `winget validate
   --manifest <folder>` first); Microsoft's pipeline installs it in a sandbox and merges it. Slate updates itself
   anyway, so a winget PR per release only matters for new installs and `winget upgrade`.

What the updater reads from a release can never change, as every version out there reads it: the tag `vX.Y.Z` (no
pre-release suffix), assets named exactly `Slate.exe` and `Slate.exe.sha256` (`<64 hex digits>  Slate.exe`), this
repository, `/releases/latest`. Slate.exe stays the x64 build. The updater never goes back to a lower version, so a
bad release is fixed by publishing a higher one; a new version that doesn't start is undone on the user's PC by the
version before it (see update.rs). A copy deployed from a local build also updates itself from GitHub once a higher
version is published (unless its automatic check is off).

Commits use the GitHub no-reply address (repo-local git config); GitHub refuses pushes that would publish another one.

## Website and images

`site/index.html` is the website at `https://jamesccupps.github.io/Slate/` (one page, no scripts from elsewhere; the
big button picks Windows or Linux from the browser): `apt.yml` publishes it with the apt repository, copying
`docs/images/*` and two icon sizes into `images/`. `docs/images` also feeds the README: `slate-demo.gif` (four frames
from Windows' test mode on a made-up 850 MB orders JSON, timings from `t:` marks in the same runs, captions drawn
with System.Drawing; the find box's text drawn in, as offscreen frames don't capture native edit boxes),
`slate-windows-dark.png`, `slate-linux.png` (test-mode shots of made-up sample files) and `social-preview.png`
(1280x640, the repository's Settings → Social preview; the user uploads it). Never put the user's own files in
them. Numbers in captions must be what was measured: an 810 MB file is editable once its lines are counted (0.13 s
there); the JSON outline for the path bar takes ~2 s more in the background and blocks nothing.

## Layout

Shared by both platforms: `src/core` (the engine), `src/highlight*` (colors), `src/edit.rs` (editing operations,
display segments, coloring checkpoints, the transform sink), `src/theme.rs` (palettes; Windows' high contrast and
accent stay Windows-only), `src/settings.rs` (data folder: `%LOCALAPPDATA%\Slate`, `$XDG_DATA_HOME/slate` or
`~/.local/share/slate` on Linux, or the portable `data` folder). Windows: `src/ui`. Linux: `src/gtk`.

- `src/core/` — the engine, no UI. Unit-tested (`cargo test --lib`). What it asks the OS for goes through `os.rs`
  (Windows and Unix sides: shared opens, reads at an offset, a second handle, stamps and file IDs — on Unix dev/inode
  and times in 100 ns units like Windows', as the session keeps them —, self-deleting temp files, free space,
  whether a process runs, which errors pass by themselves). Saving on Linux is a rename (atomic; open files keep the
  old one), the directory synced, the old file's permissions and owner kept; ANSI there is Windows-1252 from a
  built-in table (checked against Windows' own by a test on Windows). New documents get `Eol::NATIVE`.
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
    would turn characters into "?" (`Lossy`, unless the user said so); in the app also a damaged UTF-16 document
    (`Document::bad_units`, which can't be saved back as it was) asks first. Stale temp files of dead processes are
    cleaned up. After a big save the text reads exactly the bytes written, from the file written (found by its file
    ID), never what another program put at that path next.
  - `search.rs` — byte-regex search in windows with an overlap after each (a match running into a window's end is
    searched again in a bigger one): find next/prev, count all, streaming replace-all (the text between matches is
    copied from the window searched). Plain text: 8 MB windows, the overlap at least the query's length, always
    exact. A regex is exact in documents up to 64 MiB (one window; Find previous scans from the start); in bigger
    ones Count all and Replace all use 64 MiB windows with 16 MiB after (exact while no match is longer than that),
    Find next/previous 8 MB with 1 MiB. Past that, a match at a seam is missed or cut short and the matches after it
    can be wrong. Regex searches of documents over 256 KB (plain text over 32 MB) run on another thread
    (`Matcher::sync_limit`); a background find is dropped if the selection or text changed meanwhile, and Find
    previous takes its match from a finished count of the same search when there is one.
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
  - `text.rs` — encoding/EOL detection (mostly-UTF-8 with a few bad bytes stays UTF-8; UTF-16 without a BOM when
    every other byte is mostly zero and the first 8 KiB read as UTF-16 but for at most one unit in 32; the same bytes
    decide `io::looks_binary`, which for UTF-16 means a zero character), UTF-16/ANSI codecs (UTF-16 halves without
    their other half and an odd last byte read as U+FFFD and are counted in `Document::bad_units`; ANSI = the system
    code page, Windows-1252 under the UTF-8 code page option; single-byte code pages through tables made once from
    Windows' own conversions), display decoding (control chars → symbols), char classes.
  - `job.rs` — background jobs with progress/cancel, notify the UI by posting a window message (on Linux: an idle
    callback on GLib's main loop).
- `src/gtk/` — the Linux app (GTK 4): `mod.rs` the window (menu bar from a `gio::Menu`, actions with accelerators,
    the find bar's GtkEntry boxes, a notice line, native scrollbars mapping bytes, dialogs: `FileChooserNative` and
    `MessageDialog` as GTK 4.8 has them, the session written every 5 s when it changed and on closing; one Slate per
    session through GApplication/D-Bus, `HANDLES_OPEN`), `app.rs` the state and commands (`App`, `Tab`, `Cmd`; what
    needs a dialog is queued in `asks`), `view.rs` the text view on Pango and cairo (segments laid out plain and cached,
    rows of one height, colors drawn with the glyphs by `draw_row`: only those in view, each in its byte's color —
    color attributes made Pango shape minified JSON in thousands of pieces; wrapped rows hang under the indentation
    with a negative Pango indent), `chrome.rs` the tab strip and status bar (drawn like Windows'), `session.rs`
    (`session.json` and a copy per unsaved tab up to 64 MiB; bigger ones are asked about), `testmode.rs`
    (`slate --test script` under Xvfb: commands at the top of the file; shots through `WidgetPaintable`). Not on Linux
    yet: the structure panel and path bar, show whitespace, bracket matching, overtype, high contrast. Help → Open
    files with Slate… (`make_default`) makes Slate the default for the desktop file's MimeType list through GIO
    (`~/.config/mimeapps.list`; Linux lets an app do that itself). File dialogs (`FileChooserNative`) must be kept
    alive until answered (`keep_until_answered`): GTK drops them otherwise. The menu bar's items don't take the
    keyboard, and commands leave it in the text.
    `SLATE_TIMING=1` prints how long painting and keys take.
- `src/ui/` — the Win32 app (see the module docs at the top of each file).
  - `src/highlight.rs` (+ `highlight/code.rs`, `highlight/markup.rs`, `highlight/config.rs`; shared, `ui` reaches it
    as `super::highlight`) — syntax coloring for 52
    languages: hand-written lexers that color one segment and return the `State` they end in (inside a block
    comment, a multi-line string, an XML tag, a Markdown code block...). `code.rs` is one configurable lexer for
    programming/scripting languages (keyword tables + per-language extras: Rust and C++ raw strings, PowerShell
    here-strings, Batch labels, heredocs, JS and Perl regexes, Perl's `qw(…)`/`s{…}{…}` and POD, C#/HCL strings in
    interpolation holes, Dart/Scala `$x`/`${…}` in strings, Objective-C's `@` words, NSIS commands/variables/`\`
    continued lines, AutoHotkey hotkeys, VB's `Rem`...); `config.rs` the line-based formats (TOML, nginx, Apache,
    `.properties`, SRT/WebVTT, iCalendar/vCard, `.sln`, G-code, Inno Setup — whose `[Code]` section goes to
    `code.rs`'s Pascal, its state kept in the low bits of `kind` — and PPCL: Siemens APOGEE/Desigo programs, colored
    much as Desigo shows them, from the full command list of Siemens' manual; the line number dimmed, `C` comment
    lines, `# ` lines turned off and `UNKNOWN (…)` statements dimmed, `%X%` abbreviations marked, and in red what
    can't be right (a line number out of order, which the state's `b` carries; a string left open, a `GOTO` without
    a line number, `.NOT.` and other operators PPCL hasn't got, a name over 6 characters without quotes), only on
    numbered lines. Only the eleven dotted operators split a word (`ROOM.MIN.TEMP` is one name); Toggle comment puts
    the `C` after each line's number); PHP files are HTML with PHP inside. Backtick names (Kotlin, Scala, Swift) are
    plain names and Scala's `'sym` a literal; a `;` ending a G-code line is Fanuc's end of block; Markdown's inline
    spans never cross (`uncross`); errors are drawn bold (when the font has a bold face of its own: `Style::real_bold`,
    so columns stay put); in CSS an at-rule's condition (`@media (…)`, up to its `{` or `;`) is never a selector (a
    flag in the top bit of the state's `b`). A Markdown
    ``` block is colored as the language it names, that lexer's state kept in the Markdown state (`mode` holds the
    language; where its state doesn't fit, each line is colored from its line start). Where a quote is easily a
    stray one (shell, SQL, PHP...), a string left open gives up at a blank line or after 40 lines. Language is
    picked by file name (templates like `x.yaml.j2` by the name inside; nginx/Apache configuration also by its
    folder, so detection gets the whole path; `.m`/`.h` are Objective-C only with `#import`/`@interface`-style lines,
    `.m` is MATLAB too, and `.h` is C++ with `class`, `namespace`, `template`, `std::`, `public:` or `<vector>`-style
    includes in its first 4 KB), then by content (`#!` lines, `<?xml`, `server {`, `<VirtualHost`, `WEBVTT`, JSON that reads
    as JSON, PPCL's numbered lines, log timestamps — not IP addresses —, G-code by a slicer's header or lines of
    G-code words). A test checks that lexing a text in two pieces ends in the same state as lexing it whole (cut
    after a line break for every language, mid-line too for those listed in it — not Inno Setup, whose `[Section]`
    needs its line to itself); for deeper runs raise its counts for a while. Another checks that spans never cross
    and that coloring ends in the state lexing does. For speed, names and punctuation skip the checks that can't
    apply (`letter_starts_more`, `plain_punct`): a new extra for the C family, Rust, C# or SQL that starts with
    punctuation must be left out of `plain_punct` (the `plain_punctuation_starts_nothing` test only catches what the
    syntax tables say). `SLATE_HL_SAMPLES=<folder> cargo test --release --lib lexer_speed -- --ignored --nocapture`
    measures each lexer on the files in a folder.
  - `app.rs` state, layout and painting; `actions.rs` input, commands, background jobs, saving and closing;
    `editor.rs` the text view; `structure.rs` path bar + structure panel; `findbar.rs`; `session.rs` (tabs and
    unsaved text kept between runs); `settings.rs` (data folder, portable mode); `install.rs` ("Open with" entries
    in HKCU, never for what Windows runs when it's double-clicked, `NOT_REGISTERED`: .bat, .cmd, .vbs, .js, .reg,
    .py…, nor .ts — registered, they made Windows ask how to open a .bat with only Slate offered, and 0.8.2 takes
    that back once at start, a choice of Slate for .bat/.cmd included; `Slate.exe --install` does the setup
    silently for winget and scripts; Help → Stop opening files with Slate… / `--unassociate` takes back the file
    part, every UserChoice naming Slate included, and Slate stays installed; `--uninstall --quiet` removes it all;
    CI runs all three on a runner); `update.rs` (updates from GitHub releases: WinHTTP, SHA-256 check, from 0.8.1 the same
    publisher's signature when the running exe's own signature checks out (`signer_of`: WinVerifyTrust, no online
    revocation lookup), swap the exe, restart with
    `--updated`, undone if the new one doesn't start); `crash.rs` (a minidump next to crash.log on a native crash,
    written by a thread waiting for it from the start; Help → Report a problem opens a filled-in GitHub issue form
    the user sends themselves; the version string with the commit, which build.rs passes in); `prompt.rs` (prompts
    in the theme's colors, as Windows' task dialogs and message boxes stay light: a real dialog of Windows' own
    controls, push buttons themed `DarkMode_Explorer` when dark, so the keyboard, screen readers and Ctrl+C work as in
    any dialog; `win::ask` falls back to a task dialog if it can't be made; text taller than the screen is cut to whole lines,
    Ctrl+C still copies all of it; WM_GETDPISCALEDSIZE sizes it for another monitor); `testmode.rs`. Symbol-only
    buttons, status bar items and tabs (their file's path) have tooltips: `win::Tip`, Windows' own tracking tooltip,
    made the first time one shows (never in test mode), its texts from `App::tip_text`.
  - `gfx.rs` — Direct2D drawing into a D3D11 **flip-model swap chain** (FLIP_DISCARD, then FLIP_SEQUENTIAL), with
    `ID2D1HwndRenderTarget` only as a fallback: on a PC with a Parsec virtual display adapter the HWND target reported
    "occluded" and drew nothing. Device loss → `discard_target()` and paint again.
    The hardware device is made on another thread from the start (`make_device_early`; the first paint takes it,
    and Slate doesn't end before that thread does): the driver can take a quarter of a second to load. The text view
    draws through its own `IDWriteTextRenderer` (`mod text_renderer`): only the glyph runs in view (and of a long run,
    only its glyphs in view), a row's runs in one font as one Direct2D run per color, the colors given at drawing
    time through each run's cluster map rather than set on the layout (emoji through
    `ID2D1DeviceContext7::DrawGlyphRunWithColorSupport` on Windows 11; on older Windows a layout that may hold emoji
    keeps its colors on the layout and is drawn whole); a pixel test checks it draws what `DrawTextLayout` does. Bars
    and hairlines sit on device pixels (`snap`, `hline`/`vline`, `hair`), so they stay crisp at 125% and 150%.

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
  cached by (bytes, state): at most 4000 of them and 1 MB of text, never dropping one that's on screen.
- With word wrap, a line's later rows start under its indentation (after a PPCL line's number), as in VS Code,
  unless it's indented more than half the width or is over ~4 KiB (cut into segments).
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
  WM_THEMECHANGED. The caret stops blinking (shown) after Windows' caret timeout (`SPI_GETCARETTIMEOUT`), as in
  other apps, until the next key or click. A hidden system caret (`win::follow_caret`, never shown) is moved to the drawn caret after each
  paint while the text has the keyboard, with EVENT_OBJECT_LOCATIONCHANGE for OBJID_CARET; WM_KILLFOCUS destroys it
  (the find box's edit control makes its own), and so does the caret scrolling out of view. UI Automation is designed in the roadmap, not built.
- Dark mode reaches everything Slate draws, its menus (uxtheme's dark menus) and its prompts (`prompt.rs`). The Open
  and Save As dialogs are Windows' own: they follow Windows' light/dark setting, not Slate's switch.
- Status bar counts: the caret's column is worked out once per caret place (`CaretPlace`), not each frame. A
  selection up to 4 MiB is counted at once; a document up to 1 MiB at once, up to 64 MiB on
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
  is separate (its own lock, window class and session), as Windows doesn't let the two talk. A second start tries
  for up to ~10 s while the running one is starting (its window not there yet, or not ready for files) or closing
  (then it becomes the Slate once the old one has ended); files count as handed over only when the window answers 1
  (every version does once it has queued them). Guest and test-mode windows have classes of their own, so a Slate
  starting later never hands its files to them.
- Updates: the version being replaced starts the new one with `--updated` and waits ~15 s; if it can't start or ends
  with an error, it puts itself back and starts again with `--update-failed <version>` (that version isn't offered
  by the daily check again). Old copies are deleted by the next normal start. 0.2.0 starts the new one with
  `--wait-for <pid>` instead (still understood).
- Panics in message handling are caught, logged to `crash.log` in the data folder, the session is saved and a
  message shown. A panic while painting is only logged (showing a message would paint, and fail, again). A panic in
  a background job comes back as that job's failure (`job::Failure`), never as a job that runs forever.
- The keyboard focus is read with `win::focus()` when painting, not tracked from WM_SETFOCUS (which can't reach the
  App while it is borrowed). Always `win::set_focus` / `win::focus`, never SetFocus / GetFocus: in test mode the focus
  is only noted, and nothing activates the hidden window (`SWP_NOACTIVATE`, a CBT hook refusing activation), so a
  test never takes the keyboard from the user's own windows.

## Testing

- `cargo test --lib` — all unit tests, the UI's included (~210, ~5 s).
- `Slate.exe --test script.txt` drives the real app in a hidden window and renders frames offscreen into PNGs; the
  commands are listed at the top of `src/ui/testmode.rs` (`open:`, `type:`, `key:`, `cmd:`, `jobs`, `shot:`,
  `print:`, `expect:`, `answer:`, `lang:<name>` …; `print:menu0`…`menu4` lists a menu's items). In this mode
  prompts are never shown (they take answers from `answer:` lines and are listed by `print:asked`), nor are menus
  (`print:opened` lists them), the clipboard is a private one, and settings/session aren't written (unless
  `persist`, which needs `SLATE_DATA_DIR`). Mouse drags, raw wheel deltas, Alt, AltGr and window activation have
  their own commands (`down:`/`move:`/`up:`, `wheelraw:`, `altkey`, `altgr:on`, `activate`…), and `contrast:on`
  pretends Windows' high contrast is on (`print:theme`, `print:syscaret`, `print:statusbar` check the results).
  Prompts are never shown, but `prompt:<file.png>|save` (or `update`, `info`) draws one into a picture.
  `SLATE_DATA_DIR` sets the data folder, `SLATE_TEST_LOG` the log file; `session:save|soon|restore` writes the
  session now, writes it the way the timer does, or restores it. `SLATE_UPDATE_TEST_VERSION=0.1.0` makes Slate
  believe it's that version (to try the updater against the real latest release, in a scratch folder);
  `SLATE_TEST_SLOW_OPEN=<ms>` makes reading each file take that much longer (a slow network drive).
- `tests/smoke.txt` is the smoke test CI runs on the built exe (paths in it are relative to the working folder). It
  writes a session and puts it back, so it needs `SLATE_DATA_DIR` set to an empty folder.
- Measuring: `t:<label>` marks; `print:startup` (each startup step in ms since the process was created), `gfx:window`
  in a script or `SLATE_TEST_GPU=1` (draw through the hidden window's own swap chain on the GPU, as the real window
  does; `gfx:late` makes the device at the first paint, to compare), `idle:<ms>` (run the real message loop and count
  what wakes it), `print:busy` (what `jobs` waits for), `print:mem` (private bytes, working set). `copydata:<path>`
  hands a file over the way a second Slate does; `dpi:<n>` acts as if the window moved to a monitor at that DPI
  (WM_DPICHANGED); `prompt:<png>|tall` draws a prompt with more text than a screen holds; `print:tip` (after
  `hover:` and `timer:10`) the tooltip; `print:caret` whether the caret is blinked on; `down:<x>,<y>,right|middle`,
  `leave`, `print:pressed` and `print:invalidated` (repaints asked for) for the pressed states; `set:font=<family>`
  and `print:realbold`; `print:datadir` (`empty` or not). `session:`, `persist` and `crash` refuse to run without
  `SLATE_DATA_DIR`.
- `SLATE_TEST_VISIBLE=1` runs the same scripts in the real window (on top, without taking the focus), drawing
  through the real swap chain; `shot:` then captures the screen.
- Don't drive the user's desktop with real mouse/keyboard input.
- Keep small changes small: a few checks for the risky part (see the user's preference in memory).
