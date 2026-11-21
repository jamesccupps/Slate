# Slate

A fast, simple Notepad replacement for Windows that opens huge files (800 MB+ JSON) instantly. Native Win32 +
Direct2D/DirectWrite, written in Rust. Single portable `Slate.exe`, nothing to install at runtime. Since 0.8.0 also on
Linux (x86-64 and ARM64, the user's Raspberry Pi 5): the same engine, colors and editing in a GTK 4 window
(`src/gtk`), packaged as a `.deb`.

It is a general text editor first ("a better Notepad"); the JSON and XML extras (format/minify/check, the path bar and
the structure panel) only show for those files. It should be quick, easy to use, with only the features
people actually need, and as seamless as Steam/Claude-level apps. Keep this file, `docs/INTERNALS.md` (the notes
per area) and `docs/IDEAS.md` up to date as work happens. The repo is on GitHub (`jamesccupps/Slate`): keep personal paths and machine settings out of
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
- The code is formatted by hand, to about 120 columns: never run `cargo fmt` (there's no rustfmt.toml; it would
  rewrite every file). Clippy is clean and CI keeps it so on both platforms (`cargo clippy --all-targets --locked --
  -D warnings`; `[lints.clippy]` in Cargo.toml allows the three style lints the code doesn't follow); in the Linux
  Docker image it needs `rustup component add clippy` in each run. CI's Dependencies job runs `cargo deny check
  advisories licenses` (`deny.toml`: RustSec advisories, and only licenses THIRD-PARTY-NOTICES.md honors); Dependabot
  proposes crate updates monthly in one pull request (never `windows`/`windows-core`), the user merges it.
- `build.rs` writes the resource object (manifest, icon, version info) itself; there is no rc.exe/windres.
- `scripts/check-version.ps1 -Exe <exe>` runs after every release build too, after `load-config.ps1` (below): the
  version as the copies out there read it (see "What the updater reads" under Releasing).
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
   with write access) drafts the release, titled "Slate X.Y.Z", with `Slate.exe`, `Slate.exe.sha256` and the `.deb`s and tarballs (with
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

5. winget: the package is `JamesCupps.Slate` in microsoft/winget-pkgs (first submitted with 0.8.1, moved to 0.8.2 before it was merged, as 0.8.1's --install registered .bat,
   then to 0.16.1 on 2026-11-06, still unmerged: microsoft/winget-pkgs#449740, from the user's fork
   `jamesccupps/winget-pkgs`, branch `JamesCupps.Slate-0.8.1`). Its installer is the release's
   `Slate.exe`, `InstallerType: exe`, `Scope: user`, silent switch `--install`, `ProductCode: Slate Text Editor` (the
   Installed apps key; `Slate` up to 0.15.0), `UpgradeBehavior: install`. Each new version needs its own manifest PR (three files under
   `manifests/j/JamesCupps/Slate/<version>/`, with the new `InstallerUrl` and `InstallerSha256`; `winget validate
   --manifest <folder>` first); Microsoft's pipeline installs it in a sandbox and merges it. Slate updates itself
   anyway, so a winget PR per release only matters for new installs and `winget upgrade`.
6. Scoop: Slate is `slate` in the user's bucket `jamesccupps/scoop-bucket` (`bucket/slate.json`: the release's
   `Slate.exe`, its hash read from `Slate.exe.sha256`; README and website show `scoop bucket add jamesccupps
   https://github.com/jamesccupps/scoop-bucket` + `scoop install jamesccupps/slate`). The bucket's Excavator workflow
   updates the manifest from each GitHub release by itself (every 4 hours), so a release needs no Scoop work, as long as:
   - releases are tagged `vX.Y.Z` and published as the latest release (never a pre-release);
   - the file names stay `Slate.exe` and `Slate.exe.sha256` (no zip), and the `.sha256` keeps being published with it;
   - files are signed BEFORE publishing (the `sign` job does it), and a file on a release that's out is never
     replaced: Scoop and winget have recorded its hash. Fix a bad file with a new version;
   - if a file name ever has to change, `bucket/slate.json` in jamesccupps/scoop-bucket changes in the same change.

What the updater reads from a release can never change, as every version out there reads it: the tag `vX.Y.Z` (no
pre-release suffix), assets named exactly `Slate.exe` and `Slate.exe.sha256` (`<64 hex digits>  Slate.exe`), this
repository, `/releases/latest`; and Slate.exe's version where 0.16.1 and 0.17.0 look for it: the FIRST place in the
file of VS_FIXEDFILEINFO's 8-byte head (signature + structure version), which must be its version resource. Their
own code held those bytes as one constant (in `.rdata`, before the resources), so they refused every update until
0.17.1, whose `update::file_version` reads the resource section only and never holds the 8 bytes together;
`scripts/check-version.ps1` checks every build (CI's, the signed exe, `build.cmd`'s) the old way and fails it if
that doesn't give Cargo.toml's version. Slate.exe stays the x64 build. The updater never goes back to a lower version, so a
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

## Internals

`docs/INTERNALS.md` has the notes per area: *Layout* (what each module does: the engine in `src/core`, the shared
modules, `src/ui` for Windows and `src/gtk` for Linux) and *Design decisions* (why it's built the way it is). Read
the part about the area you're changing first, and keep it current in the same change. Rules every change keeps:

- Never lose work: closing keeps unsaved documents in the session; only what it can't keep is asked about, saying
  why. A save never writes over the original in place, and a failed or cancelled one leaves it as it was.
- Never touch the file system on the UI thread for opening, reloading or checking files (a network drive that went
  away mustn't freeze the window): jobs, as `Tab::load_job` and `check_disk` do.
- A dialog or menu runs a modal loop in which tabs can close, open or move: find a tab again by id afterwards, never
  by an index taken before (Windows: anything that shows one runs from the `Deferred` queue, outside the App's borrow).
- Locks guarding caches and results are taken with `.unwrap_or_else(|e| e.into_inner())`; a callback from Windows or
  GTK catches panics (`guarded`, `caught`, `catch_unwind`): one can't unwind through them.
- Test mode never shows a real dialog or window and never takes the keyboard from the user's own windows
  (`win::set_focus` / `win::focus`, never SetFocus / GetFocus).
- A new setting or session value is read leniently (one that can't be used keeps its default) and kept by older
  versions that read the file.
- Both platforms: a feature or fix in the shared code (`src/core`, `edit.rs`, `structure.rs`…) is wired into both
  apps, with a check in each test mode where it shows.

## Testing

- `cargo test --lib` — all unit tests, the UI's included (about 300 on Windows, 260 on Linux, ~5 s).
- Hostile files: `cargo test --release --lib fuzz -- --ignored --nocapture` (`src/fuzz.rs`) sends random and mutated
  text through every lexer (and in three pieces at line breaks: the same end state as whole), JSON and XML
  Check/Format, the structure scans, CSV Check and Sort, the line tools, search (queries from the input), the
  encodings (UTF-16 there and back), drawing and caret steps, Convert (Base64 and URL there and back), EditorConfig's
  globs, settings.json and shortcuts.txt, and multiple cursors; a panic or a property that doesn't hold is reported
  with its input, a
  step stuck for 10 s (a file that would freeze the window) stops the run and saves the input to
  `%TEMP%\slate-fuzz-hang.bin`. `SLATE_FUZZ_ROUNDS` (2000) and `SLATE_FUZZ_SEED` choose the inputs.
- `Slate.exe --test script.txt` drives the real app in a hidden window and renders frames offscreen into PNGs (the
  find bar's edit boxes, Windows' own controls, printed in: `print_children`); the
  commands are listed at the top of `src/ui/testmode.rs` (`open:`, `type:`, `key:`, `cmd:` (a `Cmd` name, or any
  command's name in the shortcuts file: `cmd:open`), `jobs`, `shot:`,
  `print:`, `expect:`, `answer:`, `lang:<name>` …; `print:menu0`…`menu4` lists a menu's items). In this mode
  prompts are never shown (they take answers from `answer:` lines and are listed by `print:asked`), nor are menus
  (`print:opened` lists them), the clipboard is a private one, and settings/session aren't written (unless
  `persist`, which needs `SLATE_DATA_DIR`). Mouse drags, raw wheel deltas, Alt, AltGr and window activation have
  their own commands (`down:`/`move:`/`up:`, `wheelraw:` — over the text, or `wheelraw:<delta>@<x>,<y>` over a
  point such as the tab strip —, `altkey`, `altgr:on`, `activate`…), `request:<lines, | between>` hands files over
  as a second start with `--read-only` / `--wait` / `-` does (`waitevent:<name>` makes the event, `print:waits`
  shows the waits and whether it's set), `print:findtext` the find bar's boxes and Find in selection's range,
  `print:findbar` (closed, or its mode), `print:carets` (multiple cursors, the main one starred; both test modes),
  `print:menukeys0`…`4` (a menu's items with the keys they show, the user's included: `crate::shortcuts`),
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
  `outside:<path>|<text>` (both test modes) writes a file as another program would, unseen until Slate next looks.
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
  `print:wrong`, `print:speller`, `down:<x>,<y>[,alt|ctrl][,<n>]` / `move:<x>,<y>` / `up:[ctrl]` (the left button in the text, the gutter included;
  `<n>` is GTK's click count, 2 a double-click's second press),
  `act:<action>=<value>` (a menu item with a value: `act:save-encoding=ansi`, `act:font-size=14`, `act:show-tab=2`),
  `tabclick:<x>[,<button>]` (`list` for the tab list's button; 3 for a tab's menu) and `statusclick:indent|lang|eol|
  encoding|readonly|ovr` (menus are listed in `print:asked`, not shown), `pathclick:<i>` / `rowclick:<k>[,chevron|
  double]` (the path bar and the panel), `gsettings:<key>=<value>` (GNOME's interface settings, under
  `GSETTINGS_BACKEND=memory`; the image needs gsettings-desktop-schemas), `print:encoding`, `indent`, `font`,
  `settings`, `strip`, `path`, `rows`, `shown` (the path bar, the panel and the find bar), `findlabel` (what the
  find bar says), `matches` (the matches marked), `keys-<action>` (an action's accelerators now; `key:` goes through
  the accelerators first, as GTK does), `down:<x>,<y>,ctrl` (a Ctrl+click),
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
