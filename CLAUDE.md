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
for /f "delims=" %s in ('rustc --print sysroot') do set SYSROOT=%s
set PATH=%USERPROFILE%\.cargo\bin;%SYSROOT%\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;%PATH%
cargo build --release
cargo test --lib
```

The linker's folder is taken from `rustc --print sysroot`, not spelled `%USERPROFILE%\.rustup\…`: rustc passes its
own C runtime and import libraries to that linker only when the `x86_64-w64-mingw32-gcc` found on the PATH lies
inside its sysroot as rustc sees it, with links resolved. With `.rustup` a link to another drive, the linker found
through the link path counted as someone else's and every link failed ("cannot find crt2.o").

- The `windows` crate is pinned to **0.58**: newer versions link through raw-dylib, which needs `dlltool` + an
  assembler that the GNU toolchain doesn't bundle.
- CI builds with the Rust version in `.github/workflows/build.yml` (`RUST_VERSION`, 1.99.0, as on the development
  PC): a newer one only on purpose, raised there and here together (`rustup update`), never whatever "stable" is on
  the day a tag is built. (No `rust-toolchain.toml`: it would make every local build fetch a second toolchain.)
- `build.rs` writes the resource object (manifest, icon, version info) itself; there is no rc.exe/windres.
- `scripts/load-config.ps1 -Exe <exe>` runs after every release build (CI's, and `build.cmd`'s): GNU ld doesn't fill in
  the PE's load configuration, so the script points it at `_load_config_used` in `src/main.rs`, whose
  DependentLoadFlags (LOAD_LIBRARY_SEARCH_SYSTEM32) make Windows take the DLLs Slate.exe imports from System32 only,
  never from the folder it's in (Downloads, the first time: a DLL a web page got saved there isn't loaded). It fails
  unless that structure is in the exe once, and recomputes the PE checksum. A plain `cargo build` lacks it. Timed: no
  difference at startup (the first starts of any new exe are slower while Windows scans it: warm it up first).
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
   repository secrets `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` say which app. The workflows'
   actions are pinned to commits (the version in a comment); Dependabot (`.github/dependabot.yml`) proposes newer
   ones monthly (download-artifact only runs for tags: try a new one on a throwaway branch with a workflow passing
   files the way build.yml does before the user merges it). Checkouts keep no token in `.git`
   (`persist-credentials: false`: nothing pushes; `gh` gets `GH_TOKEN`). Security reports: `SECURITY.md` (GitHub's
   private reporting).
4. Publishing the release runs `.github/workflows/apt.yml` (named "apt repository"; it also publishes the website, and
   runs again when `site/`, `docs/images/` or the apt scripts change on `main`) (it starts itself again on `main`, as
   GitHub Pages only publishes from there): `packaging/linux/apt-repo.sh` makes the apt repository from the latest
   release's `.deb`s, signed with `APT_SIGNING_KEY` (its job runs in the `apt` environment: the secret belongs
   there, the environment limited to `main`), and Pages (Source: GitHub Actions) publishes it at
   `https://jamesccupps.github.io/Slate/apt` (suite `stable`, component `main`). The Release file has no `Valid-Until`
   (since 0.12.2; 0.9.1 to 0.12.1 signed it for a year, re-signed monthly): an expiry breaks `apt update` on every
   system with Slate once nobody signs it again (GitHub stops schedules after 60 days without activity), which is
   worse than an old index served again (it only keeps Slate at that version). Every `.deb` brings the keyring
   (`packaging/linux/slate-archive-keyring.gpg` → `/usr/share/keyrings/`) and adds
   `/etc/apt/sources.list.d/slate.list` when installed (removed with the package), so `apt upgrade` updates Slate.
   The private key's only other copy is on the development PC in `%USERPROFILE%\.slate-apt-key` (never in the
   repo). The repository's URL, suite, component and keyring path can never change, as every installed Slate reads
   them. It may be signed by any key in the keyring (apt-repo.sh checks that it is one), so a second key added there,
   kept offline, can take over once the packages bringing it are installed; a key no installed keyring has makes apt
   refuse the repository until a `.deb` bringing it is installed by hand. The keyring holds the signing key
   (`543472F1…46F8`) and, from 0.9.1, a backup (`1A06C0D8…96AF`, made in a throwaway container: Git Bash's
   gpg-agent hangs on this PC), whose private half the user keeps offline.

5. winget: the package is `JamesCupps.Slate` in microsoft/winget-pkgs (first submitted with 0.8.1, moved to 0.8.2 before it was merged, as 0.8.1's --install registered .bat:
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
version is published (unless its automatic check is off). Nor can who signs it: from 0.9.1 the updater wants the
download's certificate to have the running exe's name and every identity usage it has (`1.3.6.1.4.1.311.97.…`, the
identity Artifact Signing validated; from 0.10.0 not the service's own usages under `…97.1.`, the same for everyone;
the issuing CA varies, so it isn't compared). Before ever signing with a new identity (validated again: a new name
or address, a new account), add its usage to `update::ALSO_TRUSTED` in a release signed with the old one, and wait
until most copies have it: copies without it refuse the new one ("…with another identity…, download the new version")
and have to be updated by hand once.

Commits use the GitHub no-reply address (repo-local git config); GitHub refuses pushes that would publish another one.

## Website and images

`site/index.html` is the website at `https://jamesccupps.github.io/Slate/` (one page, no scripts from elsewhere; the
big button picks Windows or Linux from the browser): `apt.yml` publishes it with the apt repository, copying
`docs/images/*` and two icon sizes into `images/`. `docs/images` also feeds the README: `slate-demo.gif` (four frames
from Windows' test mode on a made-up 850 MB orders JSON, timings from `t:` marks in the same runs, captions drawn
with System.Drawing; the find box's text was drawn in then, before `shot:` printed the find bar's edit boxes too),
`slate-windows-dark.png`, `slate-linux.png` (test-mode shots of made-up sample files) and `social-preview.png`
(1280x640, the repository's Settings → Social preview; the user uploads it). Never put the user's own files in
them. Numbers in captions must be what was measured: an 810 MB file is editable once its lines are counted (0.13 s
there); the JSON outline for the path bar takes ~2 s more in the background and blocks nothing.

## Layout

Shared by both platforms: `src/core` (the engine), `src/highlight*` (colors), `src/edit.rs` (editing operations,
display segments, coloring checkpoints, the transform sink), `src/theme.rs` (palettes; Windows' high contrast and
accent stay Windows-only: the accent in the shade Windows 11's apps use on the theme, from Windows' accent palette, a
lighter one on dark), `src/settings.rs` (data folder: `%LOCALAPPDATA%\Slate`, `$XDG_DATA_HOME/slate` or
`~/.local/share/slate` on Linux, or the portable `data` folder). Windows: `src/ui`. Linux: `src/gtk`.

- `src/core/` — the engine, no UI. Unit-tested (`cargo test --lib`). What it asks the OS for goes through `os.rs`
  (Windows and Unix sides: shared opens, reads at an offset, a second handle, stamps and file IDs — on Unix dev/inode
  and times in 100 ns units like Windows', as the session keeps them —, self-deleting temp files, free space,
  whether a process runs, which errors pass by themselves). Saving on Linux is a rename (atomic; open files keep the
  old one), the directory synced; the temp file is readable by its owner only while it's written (when it replaces a
  file), then gets the old file's owner, then its permissions (in that order: a chown clears setuid; setuid and setgid
  only when the owner and group could be kept, as `cp -p`), then its extended attributes (ACLs, SELinux labels; not
  IMA's and EVM's, the kernel's for the old content; a hard link is split, as by any rename), all before the fsync and
  the rename; ANSI there is Windows-1252 from a
  built-in table (checked against Windows' own by a test on Windows). New documents get `Eol::NATIVE`.
  - `source.rs` — immutable byte sources: memory, or a file read on demand via a 64 KiB block cache (opened with
    full sharing, never locked). Newline index = cumulative count per 64 KiB block, built in the background; a file
    that only grew (a log) reuses the old index if sampled blocks (and the old last, incomplete block) still match.
    Reads never fail: missing bytes read as zeros and bump `read_errors` (save refuses to write then). Index reads
    are tried again; one that keeps failing stops the index (`index_error`: the doc stays pending), never a guessed
    count. Each file source keeps its file's stamp through its own handle (size, write/change times):
    `changed_in_place` tells another program writing into that file (the text read from it isn't the user's any
    more) from a file that only grew or was replaced by a new one (the handle keeps reading the old one). The same
    size with a new last-write time counts as written into (a value changed to one of the same length can be
    anywhere: samples can't rule it out), as for the session's `same_as`; a new change time alone doesn't. Bulk
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
    ID), never what another program put at that path next. On Windows a file with what a renamed new one wouldn't
    have (EFS, compression, alternate streams such as the downloaded-from-the-internet mark: `place::special`) is put
    in place with ReplaceFile first, which keeps those (where it can't, the POSIX rename and MoveFileEx as for any
    file: saved, without them). Permissions: when the original's DACL isn't the one the temp file got in that folder
    (entries of its own, a protected list, or a file moved in from another folder with that folder's entries still
    marked inherited), `place::keep_permissions` copies it onto the temp file as it is (SetKernelObjectSecurity, which
    doesn't work inherited entries out again from the folder as SetSecurityInfo and ReplaceFile do; through a handle
    of its own with WRITE_DAC), checks it, and only if that fails goes the ReplaceFile way. An encrypted file in a
    folder where Windows can't make an encrypted temp file isn't saved (it would be on the disk unencrypted): the
    message says so;
    a set-aside original ReplaceFile couldn't put back is never hidden+temporary, which is what marks a deletable
    leftover. Saving as UTF-16 asks first (`Lossy`) when a mostly-UTF-8 document's stray bytes would become U+FFFD.
    `open_either` (`name.txt:120:5`) falls back to the name without the suffix on "not found" and on Windows'
    "invalid name" (a stream `120` of type `5`).
  - `search.rs` — byte-regex search in windows with an overlap after each (a match running into a window's end is
    searched again in a bigger one): find next/prev, count all, streaming replace-all (the text between matches is
    copied from the window searched). Plain text without Whole word skips the regex where that's faster, finding
    exactly what it would (`Literal`, a test compares them, `ſ` and the Kelvin sign included): Match case by
    `memmem`; ignoring case, an ASCII query whose longest run of letters and digits isn't at its start
    (`"status":"refunded"`) by that run first, then the query in a window around each place (5x faster in a JSON
    file, where the start is every record's key), but only where timing both ways on the first 256 KiB says so
    (`anchor_pays`, once per `Matcher`): for `Wool socks`, whose run is as common as its start, the regex alone is 20%
    faster. `SLATE_SEARCH_FILE=<file> cargo test --release --lib search_speed -- --ignored --nocapture` times them
    (its `count_all` line uses a test build's 1000-byte windows, `WINDOW`: not the app's speed).
    Windows are read over the same buffer (`Snapshot::read_over`; zeroing each first made counts 5% slower). Plain text: 8 MB windows, the overlap at least the query's length, always
    exact. A regex is exact in documents up to 64 MiB (one window; Find previous scans from the start); in bigger
    ones Count all and Replace all use 64 MiB windows with 16 MiB after (exact while no match is longer than that),
    Find next/previous 8 MB with 1 MiB. Past that, a match at a seam is missed or cut short and the matches after it
    can be wrong. Regex searches of documents over 256 KB (plain text over 32 MB) run on another thread
    (`Matcher::sync_limit`); a background find is dropped if the selection or text changed meanwhile, and Find
    previous takes its match from a finished count of the same search when there is one. `filter_lines_to` (View →
    Show only matching lines, Alt+L) streams the lines a match starts in (or, inverted, the others) into a `Sink`,
    with a `LineMap` (runs of lines that follow each other in both: one entry per kept stretch, at most `RUNS_MAX`)
    saying which line of the source each one is; `LineMap::through` maps a filter of a filter to the first text.
    400 MB, 809,820 lines kept: 0.23 s. Find in selection searches a range: `count_in`, `replace_all_to`'s `range`
    (a match running out of it isn't in it), find next/previous between its ends (the apps: `search_range`).
    Over 16 MB (`PAR_MIN`), Count all and the filter search parts of the text side by side (`parts_for`: one per
    core up to 8; a regex only in a text over `ONE_WINDOW`, in 4, with 32 MiB windows and the usual 16 MiB after:
    ~190 MB at most; plain text 2 MB windows; `side_by_side` sums their progress). Count all's parts are cut between
    characters and joined exactly (`count_parts`): where the last match of a part runs into the next one (a query
    that matches within its own match, "aa" in "aaa"), that part is searched again from the match's end until it
    meets one of its own matches. The filter's parts are cut at line starts: each notes its matched lines (`Kept`),
    then they're written in order from windows of the text (`Reader`); a part that finds a match with a line break
    in it (which could cross into the next part), or more stretches than its share of `RUNS_MAX`, gives up before
    anything is written and one search does it. 850 MB of JSON counted in 0.18 s (0.39 one search), "ERROR" in a
    400 MB log 0.13 s (0.23). A big read on one thread while another's has the file's bulk handle gets a handle of
    its own (`Source::bulk_read`, kept for next time): Windows runs one handle's reads one after the other.
  - `json.rs` — one streaming tokenizer for pretty-print / minify / validate (JSON Lines OK), errors with offsets;
    comments (JSONC) are refused with a message saying so. Check (`Mode::Validate`) is stricter than Format: UTF-8
    only, and several values only one per line. The characters are checked beside the parse, on a second thread
    (`beside_chars` with `text::CharCheck`, the first error in file order wins), so Check takes no longer.
  - `xml.rs` — the same for XML (well-formedness check; markup separated only by whitespace with a line break goes
    on its own line, text is kept exactly, and nothing is added inside an element once it has text, so mixed content
    keeps its meaning; whitespace-only elements, spaces between elements on one line and `xml:space="preserve"` stay).
    Check also wants XML's characters (`text::xml_char`, beside the parse as for JSON), character references to them,
    the declaration only at offset 0, and a name after `<?`.
  - `lines.rs` — sort (natural: numbers with signs and decimals, case and accents ignored), remove duplicate / blank
    lines, trim line ends, change case. Lines are (start, length) pairs into the text (8 bytes each); a big sort runs
    on up to 8 threads (runs sorted side by side, then merged); whole documents up to 512 MB and `COUNT_MAX` (32 M)
    lines. A `\r` is part of a line break only before a `\n` (a lone one at the very end is text). `apply_to` hands
    the result on in 4 MB pieces (big documents: straight into the `Sink`, never whole in memory beside the text;
    a 400 MB log's Sort peaked at 830 MB before, 490 MB since, and went from 4.5 to 2.8 s). Every pairwise merge is
    cut into pieces merged side by side too (`merge_sort`: `a` cut at even places, `b` before its first item not
    less than the one there; the last merge on one thread had been a fifth of the time): 2.0 s. Remove duplicates
    over 200,000 lines works out the lines' hashes side by side, then each line goes to one of the threads' tables by
    bits of its hash the tables don't use (`dedupe_marks`; a line whose hash is another line's goes in a set of its
    own): 1.0 -> 0.47 s, 40 MB more while it runs.
  - `csv.rs` — CSV and TSV: Check (RFC 4180's quoting; every row as many fields as the first, a quote left open,
    a stray quote in a field that isn't in quotes — TSV has no quoting rules, so there a quote is text unless it
    starts a field — or something after a closing quote, with where), streaming and jumping from one special byte to
    the next, a row without quotes counted at once (`bytecount`): 303 MB in ~0.18 s. The separator: the language's
    (`;` for "CSV (semicolons)", a tab for TSV), plain CSV's from its first rows (`sniff`, of `,` `;` tab `|`).
    `sort_by_column` (Format → Sort by this column: rows kept whole, line breaks in quotes too; dates `m/d/y` or
    `d/m/y` by value as the system writes them, `os::day_first`, worked out once per row; numbers and text by
    `natural_cmp`; empty fields last; the first row on top when it reads as a header: none of its fields a number or
    a date, `has_header`), on several threads (`lines::par_sort_by`), equal rows in their order. `field_at` gives
    the caret's field (the status bar names its column from the header: `edit::csv_field`).
    `SLATE_CSV_FILE=<file> cargo test --release --lib csv_speed -- --ignored --nocapture` times Check and a sort.
  - `jsonnav.rs` — lazy JSON structure: the children of one container (lists over 100,000 keep every 64th child
    and rescan between them; lists of containers inside a scanned one already over 1024), the path at an offset
    (`data[1203].name`), previews. Comments are skipped; in a file cut short, open containers end at its end. A scan
    can also keep the lists of every container holding an offset (`path_to`): the path to the caret, however deep,
    then needs one scan, and a walk takes the last path's steps as they are where it goes the same way. A scan stops
    at a container deeper than `DEPTH_MAX` (50,000) inside the one scanned (partial from there; stopping costs the
    scan nothing, reading on past it made every scan 3-6% slower, measured twice: in 0.9.0, and again in 0.10.0 with
    the reading past outside the loop), xmlnav reads past such an element as one child, and paths go no deeper: a file
    of nothing but `[` took ~200 bytes a byte (gigabytes, then an abort). Check and Format stop there too ("nested
    more than 50,000 levels deep"), as they keep an entry per level open. Runs of whitespace are skipped at once (a
    pretty-printed file's outline in 60% of the time; one-line JSON the same).
    `SLATE_SCAN_FILE=<file> cargo test --release --lib scan_speed -- --ignored --nocapture` (`xml_scan_speed`) times
    an outline scan.
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
    Windows' own conversions), display decoding (`display_char`, both apps and printing: control chars → symbols;
    characters with no width that change what a text means — zero-width space, word joiner, a byte order mark
    inside the text, soft hyphen — → ▯; the bidirectional controls of "Trojan Source" → ⇄, so they don't reorder
    the text on screen; the zero-width joiner and non-joiner stay, emoji and scripts need them; `is_odd_space` for
    Show whitespace's ⌴ on non-breaking and other look-alike spaces), char classes.
  - `src/print.rs` (shared, beside `core`) — printing's layout: rows of a fixed number of columns (a row ends at a
    space where it can, a word longer than a row is cut; tabs to stops counted from the line's start; control
    characters as their symbols; East Asian wide characters two columns), colored by the lexers as in the editor
    (the state carried from line to line; past 32 MiB each line on its own), at most 10,000 pages (`PAGES_MAX`,
    asked about past 100). The windows measure their font and draw: `ui/print.rs` (PrintDlgEx; the document started
    on the UI thread, where a printer's own dialog such as Print to PDF's Save as belongs; the pages drawn with GDI on
    a job's thread, each character on the columns' grid through ExtTextOutW's advances; a guard ends or abandons the
    document and lets the printer go; `Geometry` works out a page for a printer DC and `draw_page` draws one, shared
    by printing and the preview) and `gtk/print.rs` (GtkPrintOperation: the layout in begin-print, when the
    paper is known; Pango and cairo in draw-page; the settings kept while Slate runs; a "Text" tab of Slate's own in
    the dialog, `create-custom-widget`). The options are settings (`print_colors`: else black, bold and italic
    kept; `print_line_numbers`; `print_header`: the file's name and the page's number; `print_margins` in hundredths
    of a millimetre, 3/4 inch unless changed, or the printer's own margin where it's wider). Print preview on
    Windows is `ui/preview.rs`: a modal window of its own (GDI, the theme's colors; the main window waits, its
    timers and jobs going on) playing back the pages as metafiles made for the printer chosen last or the default one
    (`print::Pages`, `Last::printer`: so what shows is what prints; the default one through PrintDlgEx's
    PD_RETURNDEFAULT, which wants PD_NOPAGENUMS too, else "the parameter is incorrect": 0.14's try build found no
    printer on any PC, and crashed drawing that message, as DrawTextW reads an empty slice's placeholder pointer),
    laid out on a job's thread; its bar has the options, Page setup (PageSetupDlgW: paper and orientation kept in
    `Last` while Slate runs, the margins in the settings) and Print (on to `print_tab`); the window is never
    narrower than its bar (`min_width`: narrower, Page setup slid under Print and Close). On Linux it's GTK's (`PrintOperationAction::Preview`: the desktop's
    previewer), and File → Page setup GTK's dialog (`print_run_page_setup_dialog_async`, kept while Slate runs).
  - `src/spell.rs` (shared, beside `core`) — spell check: which words of the rows on screen are checked
    (`words_in`: all of plain text, Markdown's prose — not code blocks, `markdown_code`, inline code or links —,
    comments in code; never data files; a word is letters with apostrophes inside, not next to a digit, `_`, `/`,
    `\` or `@`, not dotted like `file.txt`, not a link's scheme, not `#tag`/`$var`, no capital after the first
    letter, at least two letters, no scripts from Hebrew on), and `Speller` (each word asked about once, up to
    100,000 remembered). The checking is the system's: `ui/spell.rs` (ISpellChecker in Windows' language, else
    its language without the region, else English; ~25 ms to make, so it's made by `TIMER_SPELL` 300 ms after the
    first paint that wants it, test mode too: its scripts run `timer:13`; ~8 µs a new word) and `gtk/spell.rs`
    (Enchant 2 loaded with dlopen, the session's language from `LC_ALL`/`LC_MESSAGES`/`LANG`; the .deb recommends
    `libenchant-2-2` and `hunspell-en-us`; without them `Spelling::Unavailable` and nothing is marked). Add to
    dictionary goes into the user's own dictionary, which other programs share (test mode only ignores). The word
    the caret is at the end of isn't marked while the text has the keyboard (being typed); tabs that can't be
    edited aren't checked. The right-click menu's first items: up to 5 suggestions (`Cmd::Suggestion`, applied only
    while the document's version is the one the menu was opened on), Add to dictionary, Ignore.
  - `job.rs` — background jobs with progress/cancel, notify the UI by posting a window message (on Linux: an idle
    callback on GLib's main loop).
- `src/gtk/` — the Linux app (GTK 4): `mod.rs` the window (menu bar from a `gio::Menu`, actions with accelerators
    in a group of their own inserted as `win` (`win_actions`: the window's own group is published on D-Bus),
    the find bar's GtkEntry boxes, a notice line (with Save as… after a save that failed), native scrollbars mapping
    bytes, dialogs: `FileChooserNative` and `MessageDialog` as GTK 4.8 has them; one Slate per session through
    GApplication/D-Bus, `HANDLES_OPEN`: `run` hands it absolute paths, as GIO reads a relative `notes.txt:120` as a
    link and drops it, and a name that isn't UTF-8 as a `file://` link; menus as on Windows: JSON's and XML's items only
    for those files and Help's Open files with Slate… or Stop opening files with Slate…, whichever applies (actions
    turned off, `hidden-when`), File → Open recent; Ctrl+1–9 and Alt+1–9 for tabs, Shift+Alt+F formats JSON or XML;
    a panic in a callback would abort (GTK can't pass it on): `Ui::guarded` catches it around the app's work (logged in
    crash.log by the panic hook, the session written, the window says so), drawing only logs; SIGTERM, SIGHUP and SIGINT
    (`stop_on_signals`: the handler writes a byte to a pipe, a thread asleep on it `invoke`s the rest on the main
    loop) and the session manager's `query-end` write the session first
    (`ending`), and logging out waits while unsaved work the session can't keep is open), `app.rs` the state and
    commands (`App`, `Tab`, `Cmd`; what needs a dialog is queued in `asks`, `Ask::Quit` only after a save that closing
    waited for, so dialogs never pile up; a tab another program changed is read again keeping its view, following the
    end of a log; "Open already?" by name, then by the canonical path once read, never asking the file system on the
    UI thread), `view.rs` the text view on Pango and cairo (segments laid out plain and cached,
    rows of one height, colors drawn with the glyphs by `draw_row`: only those in view, each in its byte's color —
    color attributes made Pango shape minified JSON in thousands of pieces; wrapped rows hang under the indentation
    with a negative Pango indent), `chrome.rs` the tab strip and status bar (drawn like Windows'; the column and the
    selection's size counted once per caret place and text; in a narrow window the indentation, the line endings and
    the language give way first; a message stands where the caret's place does, and goes when another tab is shown), `session.rs` (`session.json` and a copy per unsaved tab
    up to 64 MiB; bigger ones are asked about; Windows' rules: lenient reading per tab and value, session.json.bak,
    only copies this Slate wrote or read are deleted, copies no tab names come back as tabs, names `tab-<time>-<pid>-<n>`
    and only `tab-…txt` read, written every 5 s on another thread when it changed and in place on closing (a write
    that fails leaves the tab's last copy listed, and closing then asks about every unsaved tab), a `restoring` mark
    while the tabs are put back: still there at the next start, the session goes into `crashed-<time>/`; that and
    `damaged/` are deleted 30 days later),
    `testmode.rs` (`slate --test script` under Xvfb: commands at the top of the file; shots through
    `WidgetPaintable`). Not on Linux yet: the structure panel and path bar, show whitespace, bracket matching,
    overtype, high contrast, word counts, the Encoding / Indentation / Font menus, big unsaved documents in the
    session (README lists it all). Help → Open files with Slate… (`make_default`) makes Slate the default for the
    desktop file's MimeType list through GIO (`~/.config/mimeapps.list`; Linux lets an app do that itself); Stop
    opening files with Slate… takes only Slate's entries out of that file (GIO's reset would forget other apps' too).
    File dialogs (`FileChooserNative`) must be kept alive until answered (`keep_until_answered`): GTK drops them
    otherwise. The menu bar's items don't take the keyboard, and commands leave it in the text.
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
    `editor.rs` the text view; `structure.rs` path bar + structure panel; `findbar.rs`; `ime.rs` (input methods
    through IMM32: see Design decisions); `spell.rs` (Windows' spell checker); `session.rs` (tabs and
    unsaved text kept between runs; a path that isn't valid Unicode is kept as UTF-16 units, `os::json_path`, and
    arguments, drops and handed-over paths stay `OsString`s); `settings.rs` (data folder, portable mode); `install.rs`
    ("Open with" entries
    in HKCU, never for what Windows runs when it's double-clicked, `NOT_REGISTERED`: .bat, .cmd, .vbs, .js, .reg,
    .py…, nor .ts — registered, they made Windows ask how to open a .bat with only Slate offered, and 0.8.2 takes
    that back once at start, a choice of Slate for .bat/.cmd included; Windows lets only the person choose the app
    for a kind of file, and Default apps shows Slate for kinds no app was chosen for (Explorer still asks the first
    time; a managed PC whose policy turns that question off just opens Slate), so after setting up a note above the
    text says which usual kinds open with Slate, which Windows still asks about, which another app opens
    (`choice_of` reads UserChoiceLatest/UserChoice, `choices_note`); `Slate.exe --install` does the setup
    silently for winget and scripts; Help → Stop opening files with Slate… / `--unassociate` takes back the file
    part, every UserChoice naming Slate included, and Slate stays installed; `--uninstall --quiet` removes it all, the
    installed copy too when it's the one running, by a hidden `cmd` once it has ended (the paths in environment
    variables read with `!`, `cleanup_command`, so no folder name is taken as a command); CI runs all three on a
    runner); `update.rs` (updates from GitHub releases: WinHTTP, SHA-256 check, from 0.8.1 the same
    publisher's signature when the running exe's own signature checks out (`signer_of`: WinVerifyTrust, no online
    revocation lookup; the name, and from 0.9.1 the identity usages too; from 0.12.2 a running exe whose signature
    Windows can't check, for any reason but "not signed", installs nothing by itself: `Signature::Unverified`), swap
    the exe, restart with
    `--updated`, undone if the new one doesn't start); `crash.rs` (a minidump next to crash.log on a native crash,
    written by a thread waiting for it from the start; Help → Report a problem opens a filled-in GitHub issue form
    the user sends themselves; the version string with the commit, which build.rs passes in); `prompt.rs` (prompts
    in the theme's colors, as Windows' task dialogs and message boxes stay light: a real dialog of Windows' own
    controls, push buttons themed `DarkMode_Explorer` when dark, so the keyboard, screen readers and Ctrl+C work as in
    any dialog; `win::ask` falls back to a task dialog if it can't be made; text taller than the screen is cut to whole lines,
    Ctrl+C still copies all of it; WM_GETDPISCALEDSIZE sizes it for another monitor); `testmode.rs`; `uia.rs` (screen
    readers: see Design decisions). Tabs that don't all fit share the strip as whole tabs (as many as fit, a little
    wider than `TAB_MIN_W`), and the strip scrolls by whole tabs (a wheel notch is one), so none shows cut off at the
    edge (`App::place_tabs`). Symbol-only
    buttons, status bar items and tabs (their file's path) have tooltips: `win::Tip`, Windows' own tracking tooltip,
    made the first time one shows (never in test mode), its texts from `App::tip_text`.
  - `jumplist.rs` — the taskbar button's jump list: Open recent's files in a "Recent" category
    (`ICustomDestinationList`, written on another thread whenever the list changes and at the start, never in test
    mode or a guest window; files the user took off the list stay off, as Windows refuses a list that has them).
    On Linux the same files go in GTK's `RecentManager` (`gtk::note_recent`).
  - `gfx.rs` — Direct2D drawing into a D3D11 **flip-model swap chain** (FLIP_DISCARD, then FLIP_SEQUENTIAL), with
    `ID2D1HwndRenderTarget` only as a fallback: on a PC with a Parsec virtual display adapter the HWND target reported
    "occluded" and drew nothing. Device loss → `discard_target()` and paint again.
    The devices are made on another thread from the start (`make_device_early`; Slate doesn't end before that
    thread does): WARP first (~5 ms; made after the card's, it would wait for the driver: a DLL loaded meanwhile
    waits), then the card's, whose driver can take a quarter of a second to load. The first paint waits up to
    `GPU_GRACE` (40 ms) for the card's device, else draws with WARP (`on_warp_for_now`) and goes on to the card's at
    the paint after `WM_APP_GPU` (`switch_to_gpu`; `discard_target` lets go of the old swap chain at once: target
    unset, ClearState + Flush, as a window has one at a time). On an NVIDIA PC the first frame came at 82–98 ms
    instead of 220–265 ms. The window is shown cloaked (`win::cloak`, DWMWA_CLOAK) and uncloaked after its first
    present (or by `TIMER_UNCLOAK` after 1 s): shown before it, its blank client area flashed white. A flip-model
    frame presented while cloaked is dropped, so the first frames go through a bit-block-transfer swap chain
    (`blt_now`; WARP's all of them), whose frames land in the window's own surface and stay there when it's let go of:
    after the first frame `settle` makes a flip-model one on the same device (`next_dev`), and the switch from WARP
    shows the last WARP frame until the card's first one. (The user checked it on a real screen, 0.12.3: test mode's
    hidden windows can't show a flash.) The text view
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
  (the find box's edit control makes its own), and so does the caret scrolling out of view.
- Screen readers (`uia.rs`, UI Automation): WM_GETOBJECT for UiaRootObjectId answers a fragment root that adds one
  child to the window's own element: the active tab's text, a Document named after the tab with the Text pattern
  (ITextProvider2). Its ranges are byte offsets in a table (`RANGES`), moved by `View::sync` with each edit. Units: a
  character is what the caret steps over, a word letters and digits or punctuation with the spaces after them (each
  line starts a new one), a line the view's row (wrapped or not), a paragraph a line of the document. UIAutomationCore.dll
  is loaded only when a client first asks (no cost at startup); events (focus when another tab's text gets the
  keyboard, text selection changed, text changed, the name after Save As) go out after each message, only while
  clients listen (`uia::after`, outside the App's borrow: UI Automation may call back). Providers keep only ids: a call
  finds the App on the UI thread (`UseComThreading`), fails with UIA_E_ELEMENTNOTAVAILABLE while it's borrowed or the
  tab or document is gone, and catches panics. A range can be a whole huge file: a client gets at most 1 M UTF-16 units
  of text at once (`TEXT_MAX`), a move by characters past 4096 counts them in bulk (2 billion through 400 MB: 72 ms),
  by paragraphs uses the line index, by rows stops after 250 ms. Not yet: the tab strip, status bar and menu bar as
  elements, notifications for messages and search results, Linux (Orca).
- Dark mode reaches everything Slate draws, its menus (uxtheme's dark menus) and its prompts (`prompt.rs`). The Open
  and Save As dialogs are Windows' own: they follow Windows' light/dark setting, not Slate's switch.
- Status bar counts: the caret's column is worked out once per caret place (`CaretPlace`), not each frame. A
  selection up to 4 MiB is counted at once; a document up to 1 MiB at once, up to 64 MiB on
  another thread (after an edit only once the typing pauses: the count's snapshot ends the piece typing goes into),
  a bigger one not at all. Words are runs of non-space characters, like `wc -w`.
- `Tab::goto` is where a tab goes once its document is ready and it's the tab shown: `slate file.txt:120:5` (only
  when the path as given isn't a file: not found, or on Windows no name at all; a name with any other colon never
  becomes a new file, as it would be an alternate data stream) and Reopen closed tab (the last 20 tabs closed that had
  a file, back at their place).
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
  only backups it wrote or read itself (names it gives only: `slates_name`), and backups no tab refers to come back as
  new tabs; restoring that crashes is caught and the session set aside (`crashed-<time>\`, deleted 30 days later,
  as `damaged\` is), also a crash a panic doesn't tell (a `restoring` mark, taken away at the first paint). While editing it's written on another thread; closing writes it
  in place. A document that grew past 64 MiB keeps its last copy listed until its first list of pieces is written.
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
  found" in a folder that is means gone, and on Windows also a folder on the way that isn't there on a fixed disk of
  this PC, `os::gone_from_local_disk`: Explorer's temp folder of a file opened from a zip is cleaned up, and such a
  tab waited for ever) waits too and looks again, less often each time, as long as it's one of those; until a tab is
  back, the session keeps its entry as read.
- Command line: `slate --read-only <files>`, `slate --wait <files>` (returns once the tabs opened for it are all
  closed, or the Slate that has them ends: git's `core.editor`; with no Slate running one is started first, so
  closing the tabs and not the whole window ends the wait) and `slate -` (standard input in a new unsaved tab). On
  Windows a second start hands these over with `COPYDATA_OPEN2` (`?read-only`, `?wait=<event>`, `?text=<file>` lines
  before the files: no file name starts with `?`; a Slate before 0.13.0 doesn't take it, so the one starting opens a
  window of its own): `--wait` makes a named event `Local\Slate.Wait.<pid>.<time>` and waits on it and on the
  running Slate's process; the running one sets it when the last of those tabs closes (`App::waits`, `tab_gone`; a
  tab a duplicate open replaced passes its wait on; closing the window sets them all), and only events with that
  prefix. Standard input's text goes in `slate-stdin-<pid>-<time>.txt` in Slate's temp folder, which the running
  one reads and deletes (no other file). On Linux the same as the open request's hint
  (`slate:read-only;wait=<token>;text=<token>;`, `gtk::run_request`, `App::open_hinted`) with the text and a wait
  file in the data folder's `handed` folder: the waiting start polls until the wait file is gone or no Slate owns
  the application's D-Bus name (`NameHasOwner`).
- One Slate per user session: a second start hands its files to the running one (to a window of Slate's class whose
  process runs as the same user with the same rights: `slate_window`). A Slate running as administrator
  is separate (its own lock, window class and session), as Windows doesn't let the two talk. A second start tries
  for up to ~10 s while the running one is starting (its window not there yet, or not ready for files) or closing
  (then it becomes the Slate once the old one has ended); files count as handed over only when the window answers 1
  (every version does once it has queued them). Guest and test-mode windows have classes of their own, so a Slate
  starting later never hands its files to them.
- Updates: the version being replaced starts the new one with `--updated` and waits ~15 s; if it can't start or ends
  with an error, it puts itself back and starts again with `--update-failed <version>` (that version isn't offered
  by the daily check again). Old copies are deleted by the next normal start. 0.2.0 starts the new one with
  `--wait-for <pid>` instead (still understood).
- Panics in message handling are caught, logged to `crash.log` in the data folder (`settings::append_crash_log`:
  past 1 MB it starts over, the last one kept as `crash.log.old`; the panic hook logs every panic, a repaint's
  too), the session is saved and a
  message shown. A panic while painting is only logged (showing a message would paint, and fail, again), also one in
  the text renderer's callbacks (`caught`: DirectWrite's callbacks can't pass a panic on). A panic in a background job
  comes back as that job's failure (`job::Failure`), never as a job that runs forever.
- The JSON/XML path bar and structure panel are worked out again after an edit only once the typing pauses
  (`structure::PAUSE`, 200 ms; documents up to 256 KiB at once): a rescan per key took a frame's time on a few MB, and
  a whole background read of a big file per key. Meanwhile they show the old places: the panel keeps the edits since
  its rows were made (`Tab::sync_view` hands it every change `View::sync` takes: `Structure::edited`), so a click on
  a row goes to its value's place now (`place_now`; a value whose text was deleted says to try again), and the rows
  made again open what was open by where it is now (`note_reopen`, `Reopen`), not by its key: an array element's
  key is its index, which an element inserted before it changes. A value whose start was edited keeps its key, so
  Format (the whole text replaced) still keeps everything open by its path. The path bar's clicks still wait.
- A tab of matching lines (`Tab::filter`: its source tab's id and the `LineMap`) is read only, isn't kept in the
  session, shows the source's line numbers in its gutter and status bar, and Enter or a double-click goes to the line
  in the source tab (`go_to_source`). Filtering it again maps through to the first tab. Read only (`Tab::read_only`,
  File → Read only, on by itself when `os::can_write` says the file can't be written, kept in the session) is checked
  in `editable()`, which every edit goes through. Bookmarks are line starts in each view (`View::bookmarks`), mapped
  through the change log in `View::sync` (`edit::map_marks`, then `tidy_marks` back to line starts), kept in the
  session; Format, Minify and line tools drop them, line endings and Replace all keep them by line number. A tab of
  matching lines keeps the searches it was made with (`FilterOf::steps`, in order for a tab of lines filtered
  again): when its source tab's file is read again (a log that grew), once the lines are counted, it's made again
  on another thread (`edit::filter_again`, `TaskKind::Refilter`), its caret on its line or at the end if it was.
- Highlighted searches (the find box's Highlight, Alt+H; `App::highlights`, both apps): kept while Slate runs, drawn
  under the search's own matches in every tab, each in one of `Theme::marks` (searched in the screenful shown,
  `matches_in`); Highlight with the same search takes it away, View → Clear highlights all. With highlights, Show
  only matching lines uses one regex of them all and the box's search (`Matcher::any`: each part with its own case
  and whole-word options), its tab named "“ERROR” or “WARN” in app.log".
- Column selection (`edit::Block`, `View::block`, both apps): lines and columns (characters from the line's start,
  a tab to the next stop, a wide character two: what a monospace font shows; the mouse's column is its x over the
  font's character width). Alt+drag (`DragMode::Block`; Alt let go after it doesn't give the menu bar the keyboard:
  `App::alt_dragged`) or Shift+Alt+arrows (so Shift+Alt+Up/Down don't move the line) make it, at most
  `BLOCK_LINES_MAX` lines. Typing, Paste (a text of as many lines: one each), Backspace and Delete go through
  `edit::block_edit` (every line, bottom to top, one undo step; a line short of the column padded with spaces when
  something is put there); Copy and Cut take `block_text`. Anything else that edits or moves the caret ends it
  (`after_edit`, `after_move`; what keeps it sets it again after).
- Drag and drop of text (`DragMode::Text`, `view::Drag::Text`): a press inside the selection takes it once the mouse
  goes past the system's drag distance (SM_CXDRAG; GTK's `gtk-dnd-drag-threshold`), `View::drop_at` drawn as a caret
  in the accent color; let go, the text moves there (with Ctrl, a copy) as one undo step (`drop_text`). A press that
  doesn't move puts the caret there. Only inside Slate (not to or from other programs).
- Input methods (Chinese, Japanese, Korean; `edit::Composing`, `View::composing`, both apps): what's being composed
  is never in the document (no undo step, nothing for the session, searches or screen readers); the caret's segment
  is laid out with it put in at the caret, its units mapped to the caret's byte so everything after maps as before
  (`SegLayout::composing`, GTK's `Lay::composing`; one layout of its own, `composed`, not in the cache), its clauses
  underlined (the one being converted thicker) and the caret at the input method's cursor. Windows (`ui/ime.rs`):
  WM_IME_SETCONTEXT drops ISC_SHOWUICOMPOSITIONWINDOW, WM_IME_STARTCOMPOSITION isn't passed on (no Windows
  composition window), WM_IME_COMPOSITION reads GCS_COMPSTR/COMPATTR/COMPCLAUSE/CURSORPOS and types GCS_RESULTSTR
  as keys do (and isn't passed on either: that would send it again as WM_IME_CHAR), the candidate list goes under
  the clause being converted (CFS_EXCLUDE). Starting a composition deletes the selection, as typing would; a
  read-only tab cancels it. A click while composing types what shows first, where the caret is, then cancels the
  input method's copy (CPS_CANCEL), at the top of `handle` outside the App's borrow; keys are left to the input
  method (one may end a syllable itself with its result still on the way: finishing on the key too typed it twice).
  Linux: GTK's preedit signals (preedit-start/changed/end; attributes to clauses in `view::composing_of`, a
  background marking the clause being converted); a click resets the input context first, as GtkTextView does.
- Tabs in the title bar (`Settings::tabs_in_title_bar`, View → Tabs in the title bar, on by default; Windows'
  title bar in high contrast). Windows: `CUSTOM_FRAME` (a thread-local, as WM_NCCALCSIZE comes while the window is
  made, before there's an App, and while it's borrowed; `wndproc` answers it before looking for the App) takes the
  caption off (`calc_frame`: DefWindowProc's rectangle, its top put back; maximized, in by the frame, which reaches
  past the screen). WM_NCHITTEST (`frame_hit`): the top edge (SM_CYFRAME) resizes unless maximized, the window's
  buttons (`App::caption_rects`, 46 DIPs each at the strip's end, drawn by Slate in the theme, close red under the
  mouse, dimmed while the window isn't active) answer HTMINBUTTON/HTMAXBUTTON/HTCLOSE (so Windows 11 shows Snap
  layouts over maximize), the strip's empty room HTCAPTION (Windows moves the window, maximizes on a double-click,
  shows its menu on a right-click: the strip's old double-click for a new tab is only without the title bar). Their
  WM_NCLBUTTONDOWN/UP aren't passed on (DefWindowProc would draw Windows' old buttons); hover by WM_NCMOUSEMOVE and
  TME_NONCLIENT leave; WM_NCACTIVATE goes on with -1 (nothing to draw again). `place_tabs` keeps 3 buttons and
  `DRAG_MIN` free. Switching it on or off (`sync_frame`, also when high contrast changes) is `Deferred::Frame`:
  SetWindowPos with SWP_FRAMECHANGED outside the App's borrow. Linux: GTK makes a window's title bar as it's shown,
  so the setting counts from the next start: the strip and `gtk4::WindowControls` (the desktop's own buttons, in its
  order) are `set_titlebar`'s widget; on the strip's empty room a drag starts `Toplevel::begin_move` (past the drag
  threshold), a double-click maximizes or restores, a right-click shows the window menu (`show_window_menu`).
- Find next and previous through all open tabs (the find box's All tabs toggle, Alt+A; `App::find_in_tabs`): on to
  this tab's end, then the other tabs in order, then the rest of this one; a tab of matching lines or one whose text
  isn't there yet is passed over, Find in selection keeps to the selection, and Find previous still takes a match
  before the caret from this tab's count. Over `Matcher::sync_limit` of text in all, on another thread (`FindAllJob`,
  dropped if a text searched or the selection changed).
- Transforms (Format, Minify, line tools, Replace all, line endings) write into a `Sink`: memory until it passes
  64 MiB, then a temp file; Format refuses a result over 64 times the text plus 64 MiB (absurd nesting; when the
  text isn't valid JSON or XML, that's what it says instead: `edit::format_stopped`).
- The keyboard focus is read with `win::focus()` when painting, not tracked from WM_SETFOCUS (which can't reach the
  App while it is borrowed). Always `win::set_focus` / `win::focus`, never SetFocus / GetFocus: in test mode the focus
  is only noted, and nothing activates the hidden window (`SWP_NOACTIVATE`, a CBT hook refusing activation), so a
  test never takes the keyboard from the user's own windows.

## Testing

- `cargo test --lib` — all unit tests, the UI's included (~260, ~5 s).
- Hostile files: `cargo test --release --lib fuzz -- --ignored --nocapture` (`src/fuzz.rs`) sends random and mutated
  text through every lexer, JSON and XML Check/Format and the structure scans; a panic is reported with its input, a
  step stuck for 10 s (a file that would freeze the window) stops the run and saves the input to
  `%TEMP%\slate-fuzz-hang.bin`. `SLATE_FUZZ_ROUNDS` (2000) and `SLATE_FUZZ_SEED` choose the inputs.
- `Slate.exe --test script.txt` drives the real app in a hidden window and renders frames offscreen into PNGs (the
  find bar's edit boxes, Windows' own controls, printed in: `print_children`); the
  commands are listed at the top of `src/ui/testmode.rs` (`open:`, `type:`, `key:`, `cmd:`, `jobs`, `shot:`,
  `print:`, `expect:`, `answer:`, `lang:<name>` …; `print:menu0`…`menu4` lists a menu's items). In this mode
  prompts are never shown (they take answers from `answer:` lines and are listed by `print:asked`), nor are menus
  (`print:opened` lists them), the clipboard is a private one, and settings/session aren't written (unless
  `persist`, which needs `SLATE_DATA_DIR`). Mouse drags, raw wheel deltas, Alt, AltGr and window activation have
  their own commands (`down:`/`move:`/`up:`, `wheelraw:` — over the text, or `wheelraw:<delta>@<x>,<y>` over a
  point such as the tab strip —, `altkey`, `altgr:on`, `activate`…), `request:<lines, | between>` hands files over
  as a second start with `--read-only` / `--wait` / `-` does (`waitevent:<name>` makes the event, `print:waits`
  shows the waits and whether it's set), `print:findtext` the find bar's boxes and Find in selection's range,
  `print:rows` the structure panel's rows (`row:<n>` / `rowopen:<n>` click one, or its arrow),
  `previewshot:<png>|<page>[|width|noprinter]` the print preview drawn into a picture (laid out for Microsoft Print
  to PDF, or as it shows without a printer), `previewrun:<steps, | between>` File → Print preview run for real
  with its window hidden (the default printer, the layout's job, the bar's clicks and keys: `preview::Script`; Page
  setup's dialog is never shown, nor is Print's), `print:printer` (`default` when Windows' default printer is
  found, as the preview looks for it), `imemsg:start|comp|<text>|<cursor>|<attributes>|<clause starts>|result|<text>|end`
  (an input method's messages through the window procedure, with what IMM32 would have: `ime::FAKE`;
  `print:composing`), `clickraw:<x>,<y>` (a click's messages through the window procedure, in pixels: `click:` calls
  the App directly), `rmenu:<x>,<y>[|<label>]` (the text's menu at a point, an item chosen by its label;
  `print:popupitems` lists the last popup's items), `print:wrong` (the words spelled wrong on screen, a frame drawn
  first) and `print:speller` (the checker's language; `timer:13` makes it), `nchit:` / `ncmove:` / `ncclick:<x>,<y>`
  (client DIPs: WM_NCHITTEST, the mouse over it, a click on a window button; the buttons are only noted in
  `print:asked`, as they'd show the hidden window or end the test; a press on the caption or an edge isn't sent, as
  Windows' moving would follow the real mouse), `ncleave`, `ncactivate:0|1` and `print:frame` (title bar tabs or
  standard, and where the client area starts), `set:print_colors=false` (and
  `print_line_numbers`, `print_header`, `print_margins=l,t,r,b`), `down:`/`move:`/`up:` with `,alt` or `,ctrl`
  held (a column selection, a copy dropped), and
  `contrast:on`
  pretends Windows' high contrast is on (`print:theme`, `print:syscaret`, `print:statusbar` check the results).
  Prompts are never shown, but `prompt:<file.png>|save` (or `update`, `info`) draws one into a picture.
  `SLATE_DATA_DIR` sets the data folder, `SLATE_TEST_LOG` the log file; `session:save|soon|restore` writes the
  session now, writes it the way the timer does, or restores it. `SLATE_UPDATE_TEST_VERSION=0.1.0` makes Slate
  believe it's that version (to try the updater against the real latest release, in a scratch folder);
  `SLATE_TEST_SLOW_OPEN=<ms>` makes reading each file take that much longer (a slow network drive).
- `printpdf:<file>[|selection]` (both test modes) prints the tab into a PDF file without a dialog (Windows:
  Microsoft Print to PDF; Linux: GTK's export, which the Linux smoke test does). To look at the pages, render them
  with poppler's `pdftoppm` in a throwaway container (the PC has no poppler).
- `tests/smoke.txt` is the smoke test CI runs on the built exe (paths in it are relative to the working folder). It
  checks Format XML, Toggle comment and `name:line:col`, writes a session and puts it back, so it needs
  `SLATE_DATA_DIR` set to an empty folder. Linux's test mode also has `session:soon` (written on another thread),
  `print:session` (the session folder's files), `print:notice` and `print:blink` (whether the caret's timer runs:
  it stops after `BLINK_STOP` or with the window in the background, so an idle Slate sleeps), and
  `findkey:<combo>` / `replacekey:<combo>` (a key in the find or replace box: `up`, `alt+s`; `key:` goes to the
  text), `imemsg:start|pre|<text>|<cursor>|<clauses>|commit|<text>|end` (the input method's signals on the window's
  input context; `pre` as GTK gives a preedit), `rmenu:<x>,<y>[|<label>]` (the text's menu's spelling part),
  `print:wrong`, `print:speller`, `down:<x>,<y>[,alt]` / `move:<x>,<y>` / `up:[ctrl]` (the left button in the text, the gutter included),
  `set:print_colors=false` and the other print options.
- Measuring: `t:<label>` marks; `print:startup` (each startup step in ms since the process was created: `warp`,
  `device`, `first frame` or `first frame (WARP)` and the switch; a hidden test window gets no WM_PAINT, so the
  switch shows at the harness's next paint), `SLATE_INDEX_FILE=<file> cargo test --release --lib index_speed --
  --ignored --nocapture` (counting lines against reading alone; test mode's `jobs` adds 40–150 ms of its own),
  `gfx:window`
  in a script or `SLATE_TEST_GPU=1` (draw through the hidden window's own swap chain on the GPU, as the real window
  does; `gfx:late` makes the device at the first paint, to compare), `idle:<ms>` (run the real message loop and count
  what wakes it), `print:busy` (what `jobs` waits for), `print:mem` (private bytes, working set). `copydata:<path>`
  hands a file over the way a second Slate does; `dpi:<n>` acts as if the window moved to a monitor at that DPI
  (WM_DPICHANGED); `prompt:<png>|tall` draws a prompt with more text than a screen holds; `print:tip` (after
  `hover:` and `timer:10`) the tooltip; `print:caret` whether the caret is blinked on; `down:<x>,<y>,right|middle`,
  `leave`, `print:pressed` and `print:invalidated` (repaints asked for) for the pressed states; `set:font=<family>`
  and `print:realbold`; `print:datadir` (`empty` or not). `session:`, `persist` and `crash` refuse to run without
  `SLATE_DATA_DIR`.
- Screen readers: `uia:<op>` works on a range through the providers the way a client does (`caret`, `sel`, `doc`,
  `visible`, `point=<x>,<y>` pick one; `expand=<unit>`, `move=<unit>,<n>`, `start=` / `end=<unit>,<n>`, `find=<text>`,
  `select`, `scroll` change it; units char, format, word, line, para, page, doc), and `print:uia` (the text element),
  `uia-line`, `uia-word`, `uia-char`, `uia-para` (the unit at the caret), `uia-sel`, `uia-range`, `uia-len`,
  `uia-rects`, `uia-attrs` and `uia-events` (raised since the last time) show what a client gets. These call the COM
  objects directly; `tests/uia-client.ps1` is a real client (.NET's, in Windows PowerShell) reading the hidden test
  window through UIAutomationCore while `tests/uia.txt` idles (start it first). Focus events never reach it: Windows
  passes those on only from the window with the keyboard, which a test window never is.
- `SLATE_TEST_VISIBLE=1` runs the same scripts in the real window (on top, without taking the focus), drawing
  through the real swap chain; `shot:` then captures the screen.
- Don't drive the user's desktop with real mouse/keyboard input.
- Keep small changes small: a few checks for the risky part (see the user's preference in memory).
