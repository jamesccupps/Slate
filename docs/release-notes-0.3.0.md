## Slate 0.3.0

A reliability release: the whole app was audited (the engine, file formats and colors, the editor and window, updates and setup), and everything that could lose work, write the wrong bytes or get in the way was fixed.

**Download:** `Slate.exe` below, for 64-bit Windows 10 or 11. If you have 0.2.0, it offers this update in its status bar by itself.

### Your work is safer
- Unsaved text kept between runs can't be lost any more: a damaged session file, one written by a newer version, a second Slate running at the same time, or a crash while reopening tabs all keep it. The session is flushed to disk and written in the background, so typing in big files doesn't pause.
- A big file that another program rewrites while you edit it is never saved mixed with your edits; Slate offers to reload it instead. Log files that only grow work as before.
- Saving as ANSI asks first when characters would turn into "?" (save as UTF-8, ANSI anyway, or cancel).
- A UTF-8 file with a few bad bytes stays UTF-8, byte for byte, instead of opening as garbled ANSI.
- Read errors while Slate counts a big file's lines (a network blip, a locked file) are retried and reported, never turned into wrong line numbers.
- Big files on network shares and FAT drives can be saved.
- A Slate running as administrator keeps its own window and tabs, apart from the normal one. If Slate is too busy to take a file you open, the file opens in a window of its own that says it doesn't keep its tabs, instead of a second Slate sharing (and overwriting) the first one's unsaved text.

### Editing
- Each file's indentation is detected (tabs or spaces, and how many); TSV files and makefiles always get real tabs. *Format → Indentation* changes it.
- A typed `}` lines up with the line that opened it; Enter on an empty indented line leaves no spaces behind.
- The caret, Delete and Backspace treat emoji, flags and accented letters as one character.
- Copying with nothing selected copies the whole line, and pasting it puts it above the current line.
- Typing over a selection undoes in one step. Indenting or commenting 200,000 lines (and undoing it) takes milliseconds instead of seconds.
- Touchpad scrolling and pinch zoom at the right speed; dragging a selection past the left or right edge scrolls sideways.
- Save As adds the extension (.txt for new documents) when you type a name without one. `Slate notes.txt` for a file that doesn't exist yet opens it empty and creates it when you save.
- Go to line understands 1,234.
- AltGr characters (ś, ł, € and others) no longer trigger Ctrl+Alt shortcuts.
- Alt alone highlights the menu bar with underlined letters, like other Windows apps.

### Formats and colors
- XML Format and Minify keep whitespace that matters: inside `xml:space="preserve"`, between inline elements (`<b>big</b> <i>world</i>`), and in elements that hold only spaces (Word and SVG files rely on these).
- XML Check catches more mistakes: `]]>` in text, `--` inside comments, undeclared entities.
- JSON with comments (VS Code settings, tsconfig.json) shows its structure, and Format says it has comments instead of failing at the first `/`.
- A JSON file that's cut short still shows its structure. While you edit a big JSON file, the structure panel and path bar stay visible and open nodes stay open. Copy JSON path gives valid paths, and huge coordinate-heavy files (GeoJSON) take far less memory.
- A stray apostrophe no longer recolors the rest of a shell, Ruby, PHP or SQL file. Heredocs, PHP pages, MySQL dumps and JavaScript regular expressions are colored right; C# method calls look like calls.
- Lowercase log levels (`error`, `warn`) and syslog lines are colored, and logs or checklists starting with `[` aren't taken for JSON.
- Sort lines puts -10 before -5 and 1.25 before 1.5, and sorts accented letters next to their plain ones.
- Very long lines color quickly (a 32 MiB single line takes about a second at most).

### Window
- Tabs with the same file name show their folder, and the title bar shows the folder too.
- *Help → Keyboard shortcuts* opens the full list in a tab.
- Status-bar menus open upwards; every menu item and prompt button has its own access key (Alt+N is "Don't save").
- Switching back to Slate keeps the keyboard in the find box if that's where it was.

### Updates
- If a new version can't start on your PC, Slate goes back to the old one by itself.
- A Slate that stays open for days still finds new versions.
- Where Slate can't replace itself (for example in Program Files), it says so and opens the download page instead.

Checksum: `Slate.exe.sha256` (SHA-256 of `Slate.exe`).
