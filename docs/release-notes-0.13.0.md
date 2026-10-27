## Slate 0.13.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

CSV and TSV tools, characters you can't see made visible, Find in selection, and a few command-line options.

### CSV and TSV
- **Format → Check CSV** (or TSV): every row should have as many fields as the first, and quotes should be where CSV allows them (a field in quotes may hold commas, line breaks and `""` for a quote). A mistake is shown above the text with its line and column, and the caret goes there. A 300 MB file is checked in about 0.2 s.
- **Format → Sort by this column** (A to Z, or Z to A): put the caret in a column and the rows are sorted by it. Numbers and dates sort by value (12/31/2026 before 1/5/2027; day or month first as Windows or your locale writes dates), empty cells go last, rows with line breaks inside quotes stay whole, and a header row stays on top.
- The status bar names the caret's column from the header row, e.g. "status (4)".
- The separator is found from the file's first lines (comma, semicolon, tab or `|`).

### Characters you can't see
- Zero-width spaces, word joiners, a byte order mark in the middle of the text and soft hyphens show as ▯. The bidirectional controls used in the "Trojan Source" trick show as ⇄ and no longer reorder the text after them on screen, so what you see is the order the characters are really in.
- With View → Show whitespace, non-breaking spaces and other spaces that only look like spaces are marked ⌴ (Windows).
- **View → Character at the caret** says what a character is and its bytes, e.g. `é  U+00E9  ·  UTF-8: C3 A9` (in the file's own encoding: UTF-16's bytes, or whether ANSI can hold it). On Windows the status bar's encoding menu shows it too.

### Find
- **Find in selection** (the new button in the find box, or Alt+S): searches, counts and Replace all stay inside what was selected when you opened the find bar. The part searched is shaded, and it follows your edits.
- **Up and Down** in the find and replace boxes go through your last 20 searches and replacements.
- In a regular expression's replacement, `\n` puts in a line break (the kind the file uses), `\t` a tab, `\\` a backslash.
- A button in the find bar for **Show only matching lines** (Alt+L).

### Command line
- `slate --read-only file.txt` opens files read only.
- `slate --wait file.txt` returns once that file's tab is closed, so Slate can be git's editor: `git config --global core.editor "slate --wait"`. If Slate isn't running yet, it's started first, so closing the tab (not the whole window) is what's waited for.
- `some-command | slate -` opens what the command prints in a new tab.

### Also
- Windows: the taskbar button's menu (right-click it) lists the files you opened or saved last. On Linux they go in the desktop's recent files.
