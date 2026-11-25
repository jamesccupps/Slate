# Slate internals

What each part of the code does, and why it's built the way it is: the notes for working on an area, read before
changing it (CLAUDE.md has the rules, building, releasing and testing). Keep them current as the code changes.


## Layout

Shared by both platforms: `src/core` (the engine), `src/highlight*` (colors), `src/edit.rs` (editing operations,
display segments, coloring checkpoints, the transform sink), `src/theme.rs` (palettes; Windows' high contrast and
accent stay Windows-only: the accent in the shade Windows 11's apps use on the theme, from Windows' accent palette, a
lighter one on dark), `src/settings.rs` (data folder: `%LOCALAPPDATA%\Slate Text Editor` — 0.15.0 and older used `%LOCALAPPDATA%\Slate`, which `move_data_dir` moves at a start when no other Slate runs, not right after an update (which may yet be undone) —, `$XDG_DATA_HOME/slate` or
`~/.local/share/slate` on Linux, or the portable `data` folder; with no home on Linux, `/tmp/slate-<uid>`, used only
when it's this user's alone, else a guest window that keeps nothing). Windows: `src/ui`. Linux: `src/gtk`.

- `src/core/` — the engine, no UI. Unit-tested (`cargo test --lib`). What it asks the OS for goes through `os.rs`
  (Windows and Unix sides: shared opens, reads at an offset, a second handle, stamps and file IDs — on Unix dev/inode
  and times in 100 ns units like Windows', as the session keeps them —, self-deleting temp files, free space,
  whether a process runs, which errors pass by themselves). Saving on Linux is a rename (atomic; open files keep the
  old one), the directory synced; the temp file is readable by its owner only while it's written (when it replaces a
  file), then gets the old file's owner, then its extended attributes while it's still 0600 (ACLs, SELinux labels;
  not IMA's and EVM's, the kernel's for the old content; a hard link is split, as by any rename; each value's size
  asked first, so one over 64 KiB is kept too), then its permissions last (a chown clears setuid; setuid and setgid
  only when the owner and group could be kept, as `cp -p`; with the mode first, the hidden temp file had for a moment
  the old mode without the access list narrowing it), all before the fsync and the rename. Where the access list
  (`system.posix_acl_access`) couldn't be put on the new file, or its group couldn't though this user is in it (and
  the mode gives the group anything), people the old file kept out could read the new one: nothing is replaced,
  `SaveError::Access`, and the GTK app asks (`Ask::Access`, `Tab::access_ok`; a private security report,
  GHSA-fwvq-4jjf-qg8c, 0.18.0; test mode `set:refuse_access=true`, unit tests `place::REFUSE_ACL_HERE`). As the new
  file is this user's, a file of another user's that this one may write (a group's, an ACL's)
  or of a group this user isn't in would change hands: `io::owner_kept` says so, and the GTK app asks first
  (`SaveError::Owner`, `Ask::Owner`, `Tab::owner_ok`). The data folder is the user's only (0700,
  `settings::make_data_dir`), and text piped in (`handed/stdin-*`) 0600; ANSI there is Windows-1252 from a
  built-in table (checked against Windows' own by a test on Windows). New documents get `Eol::NATIVE`.
  - `source.rs` — immutable byte sources: memory, or a file read on demand via a 64 KiB block cache (opened with
    full sharing, never locked). Newline index = cumulative count per 64 KiB block, built in the background; a file
    that only grew (a log) reuses the old index if sampled blocks (and the old last, incomplete block) still match.
    Reads never fail: missing bytes read as zeros and bump `read_errors` (save refuses to write then; and since an edit
    can copy such zeros into text that isn't read from the file any more, a document an insert was made in after
    `Buffer::all_read_errors` went up is `Document::suspect`: saving asks first, as for `bad_units`). Index reads
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
  - `io.rs` — open (on Linux only regular files: a pipe with no writer held the reading thread for ever, a device
    read endlessly; saving over one is refused too; ≤64 MiB into memory; bigger = file-backed + background index; UTF-16/ANSI converted to UTF-8,
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
    of its own with WRITE_DAC), checks it, and only if that fails goes the ReplaceFile way; done before anything is
    written into the temp file, so a private file's new text is never readable by those its folder lets read. An
    encrypted file in a
    folder where Windows can't make an encrypted temp file isn't saved (it would be on the disk unencrypted): the
    message says so;
    a set-aside original ReplaceFile couldn't put back is never hidden+temporary, which is what marks a deletable
    leftover. Saving as UTF-16 asks first (`Lossy`) when a mostly-UTF-8 document's stray bytes would become U+FFFD.
    `open_either` (`name.txt:120:5`) falls back to the name without the suffix on "not found" and on Windows'
    "invalid name" (a stream `120` of type `5`).
  - `many.rs` (since 0.20, issue #15) — Edit → Find in all tabs and Find in files, both apps' jobs and words in one
    place: `lines_with` (the lines a search matches, counted as it goes, each once, its text cut to 300 bytes around
    the match, never inside a character, "…" where cut; only Esc from the job's context, not its progress),
    `in_files` (the text files under a folder in name order: not hidden or `SKIP_DIRS` folders, `bin` only beside an
    `obj` (.NET's output; alone it's often scripts); a link to a file read, one to a folder not followed; a file over
    32 MB, online only (Windows' recall-on-access attributes: reading would download it), or binary passed over,
    the binary check on its first 8 KB read before the rest; UTF-16 with or without a byte order mark and ANSI made
    UTF-8; 20,000 files, 20,000 results at most; the job's progress counts the files), `in_tabs` / `in_files_results`
    (the jobs: a tab of results whose first line says what was found, what was passed over, and why it ended early:
    a limit, or Esc, which keeps what was found) and `Results` (its text, where each line goes, `Place::Tab` or
    `Place::File`, and each tab's text's version, so Enter says when a line may have moved). `Running` is a search
    on its way (`App::many`): the status bar counts (`status`), Esc stops it first (before closing the find bar:
    `stop_many`), and its tab comes to the front only if the tab shown and its text are as they were when it started
    (`may_show`), else waits behind. The tab of results is read only, not kept in the session, colored as
    `Lang::Results` (Slate's own, not in Format → Language); Enter or a double-click goes there (`go_to_source`; a
    file not there any more isn't made anew); Save as makes it the file (an ordinary tab). Replace in all tabs needs
    the find bar's replace box showing (it opens it and says so otherwise: the replacement is what the box shows) and
    runs each tab's Replace all as its own task (`start_task_in` with the find bar's search in `replace_with`);
    `ReplaceInTabs` (`App::replacing`) counts them off, a tab closed meanwhile as done (`prune`, from `remove_tab`
    too), and its message says what it replaced with and which tabs it left (read only, opening, busy, closed, or
    changed meanwhile).
  - `search.rs` — byte-regex search in windows with an overlap after each (a match running into a window's end is
    searched again in a bigger one): find next/prev, count all, streaming replace-all (the text between matches is
    copied from the window searched). A regex's `\n` is `(?:\r?\n)` and a class leaving `\n` out leaves `\r` out too
    (`line_breaks_either`: a replacement's `\n` is the text's line break, so in CRLF text `\n+` → `\n` left stray
    `\r`s; matchers are shared by all tabs, so it isn't per document). Plain text without Whole word skips the regex where that's faster, finding
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
    the declaration only at offset 0, and a name after `<?`. A tag's or attribute's name over 64 KiB stops both
    (`NAME_MAX`): the open elements' and the tag's attributes' names are kept, and a crafted "name" of hundreds of MB
    would be copied into memory. `SLATE_XML_FILE=<file> cargo test --release --lib xml_format_speed -- --ignored
    --nocapture` times Check, Format and Minify in memory (the app's own timings include writing the result to disk,
    which varies a lot: 197 MB of XML formatted took 0.81-0.86 s in memory on the development PC, 1.0-1.4 s in the app).
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
    `d/m/y` by value as the system writes them, `os::day_first`: Windows' short date order, on Linux the locale's
    `D_FMT` (`day_before_month`), worked out once per row; numbers and text by
    `natural_cmp`; empty fields last; the first row on top when it reads as a header: none of its fields a number or
    a date, `has_header`), on several threads (`lines::par_sort_by`), equal rows in their order. `field_at` gives
    the caret's field (the status bar names its column from the header: `edit::csv_field`).
    `SLATE_CSV_FILE=<file> cargo test --release --lib csv_speed -- --ignored --nocapture` times Check and a sort.
  - `jsonnav.rs` — lazy JSON structure: the children of one container (lists over 100,000 keep every 64th child
    and rescan between them; lists of containers inside a scanned one already over 1024), the path at an offset
    (`data[1203].name`), previews; a key is read up to `KEY_READ_MAX` (16 KiB, `key_at`, then cut with "…") for
    the path bar, the panel and Go to line: a crafted key of hundreds of MB was read whole at every caret move.
    Comments are skipped; in a file cut short, open containers end at its end. A scan
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
    inside the text, soft hyphen, the invisible operators U+2061–U+2064, the Hangul fillers — → ▯, and so are the tag
    characters U+E0000–U+E007F (invisible copies of ASCII that hide text from a reader of the screen, "ASCII
    smuggling"; `display_char_in`, which knows what came before: in an emoji flag's sequence after U+1F3F4, England's,
    they stay, and only there does the caret step over them with it); the bidirectional controls of "Trojan Source" → ⇄, so they don't reorder
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
    the dialog, `create-custom-widget`: the gtk4 0.11 binding hands GTK the box's pointer and lets its own reference
    go, so the box gets an extra reference for GTK to take, or GTK got a freed object, logged a GLib-CRITICAL and
    showed no tab, 0.17 to 0.19.0). The options are settings (`print_colors`: else black, bold and italic
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
  - `src/structure.rs` (shared) — the JSON and XML structure: the path at the caret, the panel's rows (lazy child
    lists from `core::jsonnav`/`xmlnav`, ranges of 100, what's open kept across edits by path and place), and Go to
    line's paths (`find_path`: `data[1203].name`, `["my key"]`, `/catalog/book[3]/title`, `/rows/*[130002]`, the
    nth child, as Copy XML path writes a step in a huge list of mixed names; the nth of a name among children that all
    have it is taken directly, not counted name by name). Each app draws it:
    `ui/structure.rs` (Direct2D; it `pub use`s the model) and `gtk/structure.rs` (cairo).
  - `src/convert.rs` (shared) — Format → Convert: Base64 (either alphabet, padding optional, wrapped), URL
    encoding (`+` stays `+`), JSON string escapes (surrogate pairs), each way, up to `CONVERT_MAX` (16 MiB) at once,
    an undoable edit selecting the result; SHA-256 (its own, tested against the standard vectors) of the selection
    or the text on another thread (`TaskKind::Hash`), shown in a box with Copy.
  - `src/editorconfig.rs` (shared) — `.editorconfig` files from the file's folder up to `root = true` (globs `*`,
    `**`, `?`, `[…]`, `{a,b}`, `{1..9}`; a nearer file and a later section win; `unset`). Each glob is made into one
    regex (`glob_to_regex`, as EditorConfig's C library does: `/**/` also no folder, a `{n..m}` a group checked
    after; braces nested deeper than 32 match nothing): trying each `{a,b}` choice in turn was exponential. Never read
    on the UI thread: a file read brings it (`Opened::editorconfig`, worked out on the open job's thread), any other
    path is looked up on another thread (`App::editorconfig_for`, kept per tab with the path it's for). Applied
    (`edit::editorconfig_of`): its indentation (over the guess, unless the user picked one); for a new or empty
    document its line endings and encoding; and at saving `insert_final_newline` (`edit::final_newline`, an edit of
    its own) when it's known for that path by then. `trim_trailing_whitespace` isn't.
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
    while the document's version is the one the menu was opened on), Change every “word” to › suggestion
    (`SuggestionAll`: the places the spell check would mark that word, `spell::places_of` over each view's
    `words_at`, so not in code, a file name or a link, replaced as one undo step, `edit::replace_places`; a text over
    `CHANGE_EVERY_MAX` (8 MB) gets a whole-word, same-case Replace all on a job instead; Linux through `Ui::command`,
    as it needs the view's size), Add to dictionary, Ignore. F7 (`NextMisspelling`, since 0.20, issue #12):
    `spell::next_wrong` from the selection's end to the text's end, then round from the start, over each view's
    `words_at` (a segment at a time, so rows not on screen count too; `F7_TIME` (150 ms) and 8 MB a press, then "F7
    looks on from here": words new to the checker are slow); the word is selected and the text's menu opens below it
    for that word (`App::menu_word`, taken by the next menu whatever it shows; Linux `Ask::SpellMenu`, test mode notes
    its items in `print:asked`). F7 while the checker is still starting runs once it's ready (`App::f7_wanted`); in
    a read-only tab or one of results it says so.
  - `src/links.rs` (shared, since 0.20, issue #7) — File → Open selected file or link and the text's menu's Open
    “name”: `target_in` takes the selection, or around a place (`around`, only what `plausible` says looks like a
    name: a folder or an ending in it, or a link), `target_of` makes it an http(s) link (for the browser: Windows
    `win::shell_open`, Linux `Ask::OpenLink`) or a full path (relative to the tab's file's folder; `/` or `\` either
    way, `%VAR%` on Windows, `~` the home folder; `name:12:3` and a compiler's `name(12,3)` a line and column;
    `file:` addresses). `around` takes a link with its brackets (`link_around`) and a quoted name with spaces
    (`quoted_around`) first; `plausible` turns down abbreviations, two sentences without their space and mail
    addresses. Other schemes are refused; a name is only ever opened as a text file in a tab, never run, and never
    made anew (Linux `open_paths_as(.., false)`). `menu_label` shows the whole path for a file outside the tab's
    folder, and a link's site. A file that can't be opened shows the tab it was opened from again
    (`Tab::opened_from`, both apps), the message with its whole path.
  - `job.rs` — background jobs with progress/cancel, notify the UI by posting a window message (on Linux: an idle
    callback on GLib's main loop).
- `src/gtk/` — the Linux app (GTK 4): `mod.rs` the window (menu bar from a `gio::Menu`, actions with accelerators
    in a group of their own inserted as `win` (`win_actions`: the window's own group is published on D-Bus),
    the find bar's GtkEntry boxes, a notice line (with Save as… after a save that failed), native scrollbars mapping
    bytes, dialogs: `FileChooserNative` and `MessageDialog` as GTK 4.8 has them; one Slate per session through
    GApplication/D-Bus, `HANDLES_OPEN`: `run` hands it absolute paths, as GIO reads a relative `notes.txt:120` as a
    link and drops it, and a name that isn't UTF-8 as a `file://` link; menus as on Windows: JSON's and XML's items only
    for those files and Help's Open files with Slate… or Stop opening files with Slate…, whichever applies (actions
    turned off, `hidden-when`), File → Open recent; Ctrl+1–9 and Alt+1–9 for tabs, Shift+Alt+F formats JSON or XML (taken in the window's capture phase: GTK also tried it as File's mnemonic and warned on the terminal);
    a panic in a callback would abort (GTK can't pass it on): `Ui::guarded` catches it around the app's work (logged in
    crash.log by the panic hook, the session written, the window says so), drawing only logs; after the main loop
    `end_ui` takes the `Ui` out of its thread-local and leaves it undropped (dropped by `process::exit` with the
    thread's locals, the window's disposal ran a focus handler whose `get_ui` found the thread-local half gone: every
    quit aborted until 0.18.0; dropped earlier, GTK warns about popovers still parented), and `get_ui` uses
    `try_with`; SIGTERM, SIGHUP and SIGINT
    (`stop_on_signals`: the handler writes the signal's number to a pipe, a thread asleep on it `invoke`s the rest on
    the main loop) and the session manager's `query-end` write the session first
    (`ending`), and logging out waits while unsaved work the session can't keep is open; SIGTERM then ends Slate,
    SIGHUP and SIGINT (the terminal closed, Ctrl+C) only when there's no such work, else they ask as closing does), `app.rs` the state and
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
    only copies this Slate wrote or read are deleted, copies no tab names come back as tabs, each tab's file's stamp
    kept as Windows does (`disk_len`, `disk_modified_ms`: a change while Slate was closed is noticed; a stamp in whole
    milliseconds matches within that millisecond, `DiskInfo::same`), names `tab-<time>-<pid>-<n>`
    and only `tab-…txt` read, written every 5 s on another thread when it changed and in place on closing (a write
    that fails leaves the tab's last copy listed, and closing then asks about every unsaved tab), a `restoring` mark
    while the tabs are put back: still there at the next start, the session goes into `crashed-<time>/`; that and
    `damaged/` are deleted 30 days later),
    `testmode.rs` (`slate --test script` under Xvfb: commands at the top of the file; shots through
    `WidgetPaintable`), `structure.rs` (the path bar over the text and the panel beside it, drawn with cairo; the
    panel's left edge resizes it, `structure_width`). Menus whose items are values (encodings, indentation, fonts,
    sizes, the tab list) are stateful actions with a target (`valued` in `actions`); View's JSON/XML items and
    Format's Formatting indent are sections `sync_menu` fills for the language. The status bar's items are
    `chrome::StatusItem`s; counts (`App::doc_counts_now`) at once up to 1 MiB, on another thread up to 64 MiB once
    typing pauses. The theme follows the system's light or dark live: GTK's theme name, GNOME's `color-scheme`
    (gio::Settings kept in a thread-local; on GNOME its "default" is its Light style, so `system_dark` doesn't go on
    to GTK's prefer-dark, read once at the start) and the desktop portal's (`follow_portal_appearance`: read once
    without waiting, then its SettingChanged). High contrast likewise (`system_high_contrast`: GNOME's
    `org.gnome.desktop.a11y.interface high-contrast`, the portal's `contrast`, a HighContrast GTK theme): then
    `Theme::high_contrast_fixed` (black on white or white on black, by the desktop's dark), selected text drawn again
    in `selection_text`, the active tab outlined; test mode: `set:high_contrast=true`, `print:contrast`. Selections
    made any way become the primary selection (middle-click paste: `refresh_now`, the mouse's on release). Close other
    / saved / all tabs (`App::closing`, `close_next`) close one by one, an unsaved one asked about first (Save as
    answered, then on; Cancel stops). Not on Linux yet: big unsaved documents in the session (README lists it all). Settings → Open files with Slate… (`make_default`) makes Slate the default for the
    desktop file's MimeType list through GIO (`~/.config/mimeapps.list`; Linux lets an app do that itself); Stop
    opening files with Slate… takes only Slate's entries out of that file (GIO's reset would forget other apps' too).
    File dialogs (`FileChooserNative`) must be kept alive until answered (`keep_until_answered`): GTK drops them
    otherwise. The menu bar's items don't take the keyboard, and commands leave it in the text. Left and Right in an
    open menu go to the next menu by Slate's own code (`menus_beside`): GTK closes one popover and opens the other in
    the same step, which on Wayland fails ("Tried to map a grabbing popup with a non-top most parent") and left the
    menus and clicks in the text dead; `open_menu_soon` opens the next 120 ms after (30 ms never worked in the VM, 60
    not always), checks it showed, and tries again if not; arrows meanwhile move the target (`MENU_NEXT`).
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
    (Slate's own keys are named "Slate Text Editor": `Software\Slate Text Editor`, the same name in
    RegisteredApplications (`registeredAppUser=Slate%20Text%20Editor`), `*\shell\Slate Text Editor`, `Uninstall\Slate
    Text Editor`; 0.15.0 and older used plain "Slate", and a test build of 0.16.0 `JamesCupps`, which `move_names`
    (`OLD_NAMES`) copies over with RegCopyTreeW and deletes, only what's Slate's, at a start with no other Slate
    and at `--install`; the ProgID `Slate.TextFile` stays, as renaming it would undo people's Default apps choices;
    "Open with" entries
    in HKCU, never for what Windows runs when it's double-clicked, `NOT_REGISTERED`: .bat, .cmd, .vbs, .js, .reg,
    .py…, nor .ts — registered, they made Windows ask how to open a .bat with only Slate offered, and 0.8.2 takes
    that back once at start, a choice of Slate for .bat/.cmd included; Windows lets only the person choose the app
    for a kind of file, and Default apps shows Slate for kinds no app was chosen for (Explorer still asks the first
    time; a managed PC whose policy turns that question off just opens Slate), so after setting up a note above the
    text says which usual kinds open with Slate, which Windows still asks about, which another app opens
    (`choice_of` reads UserChoiceLatest/UserChoice, `choices_note`); `Slate.exe --install` does the setup
    silently for winget and scripts; Settings → Stop opening files with Slate… / `--unassociate` takes back the file
    part, every UserChoice naming Slate included, and Slate stays installed; `--uninstall --quiet` removes it all, the
    installed copy too when it's the one running, by a hidden `cmd` once it has ended (the paths in environment
    variables read with `!`, `cleanup_command`, so no folder name is taken as a command); CI runs all three on a
    runner); `update.rs` (updates from GitHub releases: WinHTTP, SHA-256 check, from 0.16.1 the download's own
    version (its VS_FIXEDFILEINFO, `file_version`) must be the release's, so a release can't hand out an older signed
    build under a higher tag; from 0.8.1 the same
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
  WM_THEMECHANGED. Windows' *Underline access keys* (`actions::keyboard_cues`, SPI_GETKEYBOARDCUES, read again on
  every WM_SETTINGCHANGE) underlines the menu bar's letters all the time (`App::keyboard_cues`; test mode
  `set:keyboard_cues=true`). The caret stops blinking (shown) after Windows' caret timeout (`SPI_GETCARETTIMEOUT`), as in
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
  A save to the document's own file looks once more first, in its job (`io::changed_outside`: the stamp against
  `Document::disk`): a change since then that nobody was told of (`seen_disk`) stops it with `SaveError::Outside`
  and the notice; told, a second save keeps the user's version. (Before 0.19.2 a change in the 2 s before a save was
  written over without a word.)
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
  prefix. Standard input's text is streamed into `slate-stdin-<pid>-<time>.txt` in Slate's temp folder (never whole
  in the sending process's memory: `stdin_to_file`), which the running one takes (`take_text_file`, no other file):
  up to 64 MiB read into a new tab and deleted; bigger, opened as the file it is (read like any big file) and deleted
  when its tab closes (`handed_text`). On Linux the same as the open request's hint
  (`slate:read-only;wait=<token>;text=<token>;`, `gtk::run_request`, `App::open_hinted`) with the text and a wait
  file in the data folder's `handed` folder (`stdin-<token>.txt`, the same 64 MiB rule; what's there over a day,
  never taken, is deleted at the next start unless a tab reads it, `prune_handed`: piped text can be a secret): the
  waiting start polls
  until the wait file is gone or no Slate owns the application's D-Bus name (`NameHasOwner`), and is forgotten, not
  dropped (a GApplication registered without running warns on the terminal when dropped: `git commit` showed it).
  `--version`, `--help` (`cli.rs`; Windows prints to the console it was started from or where its output was sent,
  `print_console`) and `--`, after which everything is a file. A switch Slate doesn't know: Linux says so and ends
  (exit 2; GApplication's would end with its own help), Windows says so on the console and leaves it out, as before
  (only `--` ones: `-name` stays a file there).
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
- Locks that guard caches and results (`core/source.rs`, `job.rs`, `gfx.rs`…) are taken with
  `.unwrap_or_else(|e| e.into_inner())`: their data stays valid after a panic elsewhere, and an `unwrap` would make
  every later paint panic on the poisoned lock. A job whose thread can't be started fails as the job (`NO_THREAD`).
- Panics in message handling are caught, logged to `crash.log` in the data folder (`settings::append_crash_log`:
  past 1 MB it starts over, the last one kept as `crash.log.old`; the panic hook logs every panic, a repaint's
  too), the session is saved and a
  message shown. A panic while painting is only logged (showing a message would paint, and fail, again), also one in
  the text renderer's callbacks (`caught`: DirectWrite's callbacks can't pass a panic on), the print preview's and the
  prompt's window procedures and the menu bar's hook (each body in `catch_unwind`, Windows' own answer given), and
  GTK's print callbacks (begin-print's cancels the printing). A panic in a background job
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
  `BLOCK_LINES_MAX` lines and out to column `BLOCK_COLS_MAX` (100,000: the mouse's column stops there, and
  Shift+Alt+arrows from a caret further in say `BLOCK_TOO_FAR`, see `edit::block_at`). Typing, Paste (a text of as
  many lines: one each), Backspace and Delete go through `edit::block_edit` (every line, bottom to top, one undo
  step; a line short of the column padded with spaces when something is put there); Copy and Cut take `block_text`
  (up to `COPY_MAX`; past it nothing is copied, or cut). Anything else that edits or moves the caret ends it
  (`after_edit`, `after_move`; what keeps it sets it again after). The column helpers (`pos_at_col`,
  `block_ranges`, `block_edit`) read a line only as far as the column they're after, in pieces (`walk_cols`): the
  paint calls them for every line of it on screen, and the line can be hundreds of MB.
- Drag and drop of text (`DragMode::Text`, `view::Drag::Text`): a press inside the selection takes it once the mouse
  goes past the system's drag distance (SM_CXDRAG; GTK's `gtk-dnd-drag-threshold`), `View::drop_at` drawn as a caret
  in the accent color; let go, the text moves there (with Ctrl, a copy) as one undo step (`drop_text`; it copies the
  text, so more than `COPY_MAX` says `TOO_MUCH_TEXT`, as Duplicate does). A press that doesn't move puts the caret
  there. Only inside Slate (not to or from other programs).
- Brackets (both apps): the pair at the caret is highlighted (`edit::matching_bracket`); Edit → Go to matching bracket
  (Ctrl+M, `bracket_jump`: to the same side of the partner, so again comes back; with no bracket next to the caret,
  the closing one of the pair around it) and Select inside brackets (Ctrl+Shift+M, `bracket_select`: inside, then the
  brackets too, then inside the pair around those; `enclosing_brackets`). Brackets in strings and comments count, up
  to 1 MB away (`BRACKET_REACH`). Sublime Text's keys (asked for by a user who comes from it).
- Settings → Close Slate with the last tab (`Settings::close_with_last_tab`, off by default): Ctrl+W or a tab's × on the
  only tab marks it (`App::quit_with_tab`); once it's gone (after its question, or its save) `remove_tab` closes
  Slate (Windows `Deferred::Cmd(Cmd::Exit)`, Linux `Ask::Quit`). Close all/other tabs don't (an empty tab stays).
- 0.20's settings and places (issues #8 to #14):
  - Your own languages (#13): `Settings::languages` (`{"mmd": "Markdown", "Jenkinsfile": "Groovy", ".env": "INI"}`)
    is given to `highlight::set_user_langs` at the start and when Format → Language → Always open … changes it
    (`toggle_lang_for`, by a `highlight::LangKey`: the file's ending, or its whole name when it has none, a dot-file
    too); `Lang::detect` asks it first, by the whole name and then the ending (a key `.x` is both the file `.x` and
    the ending). The item is offered only where it would change something (`own_choice`: not "Always open .ts files
    as TypeScript"), or to turn one off.
  - Go back / forward (#11): `edit::Nav` keeps the places the caret jumped from (another tab, or more lines away than
    `NAV_LINES` and than the window shows (Page Up and Down scroll), or 64 KB), noted after every message (Windows'
    `wndproc`), every refresh (Linux's `refresh_now`) and every test-mode step; byte offsets, moved by an edit since
    in their tab (a change in its length, taken to be at the caret, as typing, deleting and pasting are), so they stay
    on their lines; only the caret comes back. Reopen where you left them: `Settings::places` (300 files, newest
    first, by the absolute path in any case on Windows: `place_key`) noted as a tab with a read file closes and
    written with the settings (at the latest at exit: a crash loses that run's), given to a file opened again as
    `Goto::Place` (Linux `restore_at`), never inside a character or a line break (`char_start`, both apps, and for
    a file read again after a change too), not to session tabs (they have their own place).
  - Hide the menu bar (#10): Windows lays it out at height 0 unless `menu_bar_shown` (Alt armed it, or a menu is
    open: `menu_bar_changed` lays out again at each change); Linux hides the menu row and `hidden_menu_keys` shows
    it for Alt on its own (the first menu) or Alt and a menu's letter (not while the find box has the keyboard).
    Those menus open once the keys are up, so on GNOME a submenu opened by hovering is refused (its grab's serial
    is a key's from before the menu had the keyboard) yet stays shown to GTK, and its item then did nothing:
    `refused_submenus` hides such a submenu again, so a click (a new serial) or the keyboard opens it.
  - Linux menu bar after a submenu's item is chosen (0.19 too, found with the 0.20 beta; GNOME): as the submenu went,
    GTK took the pointer's last place in it for a place in the window; at some heights that was the menu bar, whose
    item there opened its menu (the closing one still counted as open), then the other, without end (the window
    busy, looking frozen; GDK: "Tried to unmap the parent of a popup"). `menus_close_quietly` makes the menu row
    ignore the pointer from a click or key in a menu (seen before GTK acts on it: `each_menu` gives each menu and
    submenu its own controller, as a menu's events stop at it while it holds the pointer), when the pointer was last
    seen in a menu (only then can its place be taken for the window's; one resting on the menu bar leaves it alone),
    until the pointer really moves (a motion repeating the last place doesn't count) or something outside is clicked.
    The text's menu and the tab and status bar menus slide their submenus in (no popups of their own), so neither fix
    is needed there. With the menu bar hidden, Alt held for something else (a click or drag with it, the wheel, Alt+Tab
    away and back) opens nothing on its release (`alt_alone` cleared).
  - Find bar at the bottom (#9): `Settings::bars_at_bottom`. Windows puts `r_find` then `r_notice` above the status
    bar and draws their border on their own top row (just above it, the text would paint over it); Linux reorders the find box and the notice after the scrollbar
    (`place_bars`, CSS `.slate-bottom`). The text's top stays where it was as they open and close.
  - The Settings menu (#14): Windows' fifth (Alt+S; `MENU_TITLES`, Help is the sixth), Linux's `_Settings`.
  - Interface text size (#8): `Settings::ui_size` (0: the system's). Windows multiplies the bars' heights, the tab
    widths, the UI fonts and the find bar's sizes by `App::uik` (`ui_scale`: the size picked, else Windows' Text size
    from `HKCU\Software\Microsoft\Accessibility\TextScaleFactor`, read again at WM_SETTINGCHANGE); test mode uses
    100% unless `uisize:` says otherwise. The path bar and the structure panel (`structure::set_scale`: rows, their
    indent, the buttons) grow with it, and tooltips and prompts by the size against Windows' own (`win::ui_rel`); the
    drop-down menus are Windows' own and follow its Text size only. In the find box, toggles that don't fit beside some
    text give way, the last first. Linux scales the tab strip, the status bar, the path bar and the panel (their font
    too: `structure::set_scale`) and their fonts (`chrome::ui_k`), and GTK's own parts through `gtk-font-name` (the
    desktop's font, sized); with 0 the desktop's own text scaling applies, as it does to every GTK app.
- Multiple cursors (Edit → Multiple cursors, issue #5; `Settings::multi_cursor`, off by default: its menu items only
  say how to turn it on, its keys aren't bound (Windows `key_cmd`, GTK `set_accels`: `shortcuts::CARET_COMMANDS`)
  and do what they did, Ctrl+click stays an ordinary click). Above/below are Ctrl+Shift+Up/Down, not Ctrl+Alt:
  GNOME switches workspaces with those (found on the VM's desktop), some Windows graphics drivers turn the screen. `View::carets` (`edit::Carets`) holds the cursors
  besides `sel`, the newest last; the main one (`sel`, which the view reveals and moves as always) is the newest of
  all, so Remove last cursor pops. `edit::multi_edit` does a key's edit at each, in the text's order with a running
  offset (a cursor's range is clipped to where the one before ended), as one undo step that typing continues; cursors
  that meet become one (`Carets::tidy`, the newer kept). `move_carets` moves the others by characters and lines
  (Up/Down by column in the line, not by screen row), and for Up, Down, Home and End the main one too, by the same
  rule (the view's would go by screen rows with wrap on, and Home stop at the indentation while the others went to
  column 0: typed text landed at two columns); Home goes where the indentation ends, then the line's start, at each.
  Ctrl+double-click (triple) adds the word (line) as one more selection (`add_caret_word`); Ctrl+Up/Down scroll and
  keep them. Keys and commands that don't work with several (Page Up,
  Ctrl+Home, a format…) leave only the main one first (`exec`'s `keeps`). The cursors are for one document
  version (`Carets::version`): `View::sync` drops them if the text changed otherwise. Undo and Redo put them back
  (since 0.19.3, issue #5): an undo step keeps the other cursors' selections before and after it
  (`Step::more`, given by `Document::begin_more` / `end_more`; a typing run continues only while all of them are
  where it left them), and `edit::carets_after_undo` takes them from `undone_more` / `redone_more`. Copy joins the selections with
  line breaks; Paste puts a line at each when the text has as many (`paste_parts`). Up to `CARETS_MAX` (10,000);
  Add next match looks 64 MB each way, Select all matches works in texts up to 64 MB. Each app draws the others'
  selections and carets for the rows on screen only; with none, nothing runs (`is_empty` checks).
- Keyboard shortcuts the user changes (Settings → Change keyboard shortcuts…, `crate::shortcuts`): one list of the
  commands for both apps (the GTK app's action names, the keys each has, whether it works in the find bar's boxes
  on Windows) and the user's `shortcuts.txt` in the data folder, made the first time it's opened with every command
  on a `#` line. A line without the `#` gives that command its own keys (or none), and those keys stop doing
  anything else (`Keymap`); a line that can't be used is told on its tab when it's saved (`shortcuts_saved`), and
  changes nothing: its command keeps the keys it had (a line with a typo used to leave it with none). Keys are split at
  commas, but one right after a `+` is the comma key (`split_keys`); a byte order mark is skipped; on Windows `?` is
  read as Shift+`/` (keys are told by place there), and Alt with a menu's letter given to a command wins over the
  menu (Alt, then the letter, still opens it). Each
  app keeps its own keys as the defaults and lays the user's over them: Windows `App::key_cmd` (the user's first,
  then `global_key` / `editor_key` unless the command or the keys were given away; `commands::IDS` names the
  commands, `commands::keyed` puts the user's keys in the menus), GTK `set_accels` (`commands()`'s accelerators
  with the user's over them, again after the file is saved). Unit tests on each platform check the list's keys are
  the ones the app has. Moving and typing keys (arrows, Home, Backspace, Ctrl+1–9) aren't in it. Linux's Alt+Up and
  Alt+Down became the move-line actions' accelerators for it (GTK runs the window's accelerators before the text
  has the key; test mode's `key:` does the same: `action_for_accel`).
- Settings → Find bar (issue #6; both off by default). *Close when you click in the text*
  (`Settings::find_close_on_leave`): the keyboard going from the bar to the text closes it (Windows: where the app
  moves it there, Tab from the find box, a structure jump: `in_find_bar` first; Linux: the window's `focus-widget`
  going from inside `find_box` to the text, the close on an idle). A left press in the text or gutter closes it
  once the clicks are over: when it's let go (`close_find_on_up` / `close_find_on_release`, the tab's id;
  `close_find_after_click`) a timer of the double-click time starts (Windows `TIMER_FIND_CLOSE`, GetDoubleClickTime;
  Linux a glib timeout, GTK's `gtk-double-click-time`; `close_find_later`), and the next press in the text stops it
  and goes on with the run, so it closes after the last click (`close_find_now`; not when the keyboard went back to
  the bar meanwhile). The bar is above the text: closing it at the press (0.19.0) moved the text up under a held
  button and the drag (Windows' own mouse move as the bar's boxes hid was enough) selected from the click to what
  was under the mouse then (issue #6), and closing it at the release (0.19.1 before its audit) moved the text
  between a double-click's two presses, by whatever the scroll-back below couldn't give back (a bar that isn't a
  whole number of rows; nothing above at the top of the file), so the second press hit another line. Closing, the
  view scrolls back as many rows as the bar was tall, so the text stays about where it is on screen (not at the
  very top: no rows above); the clicks have landed by then. *One for each tab* (`Settings::find_per_tab`): `App::find_owner` is the tab the bar's
  state is about; when another tab shows (Windows `activate`, Linux `Ui::refresh`, so every way of switching
  counts) `follow_find_bar` keeps the bar's state on the old tab (`Tab::find_bar`; Go to's isn't kept) and takes
  the new one's (a new tab's: closed), without taking the keyboard from the text; the search is shared. Turning it
  on or off forgets the kept states.
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
- Tabs in the title bar (`Settings::tabs_in_title_bar`, Settings → Tabs in the title bar, on by default; Windows'
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
- The find bar's count ("3 of 120", the scrollbar's marks; Linux marks the matches on screen from it too) is counted
  again after an edit while the bar is open, and the last count stays shown until the new one is in (Windows:
  `Search::found_at` older than the text; Linux: `found`'s version), so the message doesn't blink with every key
  typed (issue #6, up to 0.18.0 it emptied at each key). Meanwhile it says only how many ("120 found": the places
  may have moved), Find previous doesn't use its places, Linux searches the screenful directly for the marks, and
  Windows' status bar shows the count's progress only after 700 ms. A Linux text over 16 MiB isn't counted again
  after an edit: its count goes, rather than staying wrong.
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
