## Slate 0.18.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

A security fix on Linux, three things a user asked for, and the rest of an outside review's small items.

### Security (Linux)
- Saving a file with an access list (POSIX ACL) that the new file couldn't be given (a network or FUSE file system, or a security policy that refuses it) no longer says "Saved" while the list is gone, which let people it kept out read the file. Slate replaces nothing and asks first; "Save anyway" saves with the plain permissions. The list now also goes on before the file's permissions, so the hidden file a save writes is never readable by more people than the original for a moment. Reported privately (GHSA-fwvq-4jjf-qg8c); thank you to the reporter.

### New, on Windows and Linux
- **Edit → Go to matching bracket** (Ctrl+M): from a bracket to the one it pairs with, and back; inside a pair, to its closing bracket.
- **Edit → Select inside brackets** (Ctrl+Shift+M): what's between the brackets around the caret; again, the brackets too; again, the pair around those.
- **View → Close Slate with the last tab**: closing the only tab (Ctrl+W, its ×) closes Slate too, after asking about unsaved changes as usual. Off unless you turn it on.
- Windows: with *Underline access keys* on in Windows' settings (Accessibility → Keyboard), the menu bar's letters are underlined all the time, not only after pressing Alt.

### Fixes, on Windows and Linux
- The invisible "tag" characters (U+E0000–U+E007F), which can hide text that a program reading the file sees and a person reading it doesn't, show as ▯ like other invisible characters, and the cursor stops at each one; the flags of England, Scotland and Wales, which are made of them, still show as flags. So do the invisible operators of mathematics and the Hangul filler characters.
- Convert → URL decode and JSON unescape no longer take `%+A` or `\u+123` as valid (a `+` isn't a hex digit).
- Go to line with an XML path reads back what Copy XML path writes in a long list of mixed elements (`/rows/*[130002]`), and goes to `/rows/row[2000000]` at once when all of the list's elements share that name.
- An `.editorconfig` section like `[{a,b{1..3}}]` now matches the file `a`.
- A JSON key or XML name of hundreds of megabytes (in a file made for it) no longer slows the path bar and structure panel down, or makes Check and Format copy it into memory: keys are read up to 16 KB for the path bar, and XML Check and Format stop at a name over 64 KB.
- Rare internal errors in the print preview, the prompts, the menu bar and printing on Linux are caught and logged, as elsewhere, instead of ending Slate.

### Linux
- Quitting Slate no longer ends in a crash at the very last moment ("Aborted (core dumped)" when started from a terminal, and a line in Slate's crash.log every time). Nothing was lost: the session and settings had been written by then.
- Sort by this column (CSV) reads ambiguous dates in the order your locale writes them (Japanese, Chinese, Hungarian, Canadian English and others were guessed wrong).
- Text piped into Slate (`some-command | slate -`) that never reached a window (Slate closing at that moment, say) is deleted at the next start after a day, instead of staying in Slate's folder.
- A middle-click paste that arrives after you switched tabs or edited the text no longer lands in the wrong place.
- F5 (time and date) no longer starts the `date` program.

### Behind the scenes
- Many more random-input tests (CSV, search, line tools, encodings, Convert, EditorConfig, settings), clippy and a check of the crates' known security advisories and licenses in every build, and Dependabot proposing crate updates monthly.
