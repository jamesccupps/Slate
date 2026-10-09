## Slate 0.7.0

A speed and polish release. The whole app was audited again, in five parts: the engine, colors, the text view, the window around it, and starting, the session and updates. What was slow, wrong or rough was fixed.

**Download:** `Slate.exe` below, for 64-bit Windows 10 or 11. If you have 0.2.0 or newer, it offers this update in its status bar by itself (*Help → Check for updates* asks right away).

### Faster
- Slate now draws only the text in view, with its own text renderer. Long lines packed with colors (minified JSON, XML or JavaScript) draw 10 to 100 times faster: moving the caret in a 4 MB line of JSON takes 7 ms instead of 60 ms with word wrap, and 9 ms instead of 2 seconds without it.
- In an 826 MB JSON file on one line, scrolling and typing take 5–10 ms (25–40 ms before with word wrap, close to a second without it). Ctrl+End, Go to line, Select all, zooming and resizing the window are 5–10 times faster, and memory while paging through it stays under 100 MB (it reached 2 GB).
- Coloring code is about twice as fast, so jumping to the end of a big code file is quicker (a 30 MB C file: 0.14 s instead of 0.25 s).
- ANSI files open up to 4 times faster and save twice as fast; UTF-16 files open twice as fast.
- Replace all is up to twice as fast, sorting lines about a third faster, and JSON Format, Check and Minify 15–25% faster.
- The window shows up a little sooner: the graphics driver loads while the rest of Slate starts.
- An idle Slate rests: the caret stops blinking a few seconds after the last key or click, as in other Windows apps, and the look for files changed on disk every 2 seconds no longer redraws the window when it finds nothing.

### Fixed
- Find: a regular expression whose match ran over 64 KB (a long string in JSON, say) could be missed where the search crossed one of its 8 MB steps, and false matches reported after it; Replace all acted on those. Matches up to 1 MB are now always found as in a search of the whole text at once.
- Regular-expression searches no longer freeze the window in files up to 32 MB: they run in the background, as in bigger files.
- A file without a byte order mark that only looked like UTF-16 (often a binary file) is kept byte for byte, as ANSI files are, instead of having its odd bytes replaced when saved.
- After saving a big file, a line another program added to it right after the save no longer becomes part of your text.
- After a restart, the note saying which unsaved changes couldn't be brought back (after a power cut, or a file that's gone or doesn't answer) stays on screen, also when Slate starts by opening a file; it used to vanish at once.
- A file opened while Slate is closing (at its *Save changes?* prompt, say) opens in the next Slate instead of getting lost, and a Slate started while another one is closing waits for it, then opens with your tabs, instead of in a window that keeps nothing.
- On a nearly full disk, the session no longer fills it to the last byte, and still keeps the unsaved changes that fit.
- Colors: Markdown emphasis next to `code`, Scala symbols, names in backticks in Kotlin, Scala and Swift, CSS inside `@media` and nested SCSS, SQL's `CREATE TABLE`, web server logs, G-code's `;` at the end of a block, and C++ headers (`.h` files with classes or templates are C++, not C).

### Window
- Crisp lines at 125% and 150% scaling.
- Tooltips on the buttons that show a symbol, on the status bar's items, and on tabs (the file's path).
- Buttons look pressed while you hold them. The structure panel has a scrollbar.
- Narrow tabs show more of their names.
- With word wrap, a long line's later rows start under its indentation (and after a PPCL line's number), as in VS Code.
- Undo and Redo of a change that's out of view bring it to the middle of the window.
- Prompts with a lot of text keep their buttons on screen, and size themselves right on a monitor with other scaling. So do the find bar's boxes.
- Easier to read: dim text, warnings and dark mode's comments are more legible, and errors are red and bold, so they can't be taken for a value.
- The window's border is Windows' own again, so it shows your accent color if you've turned that on.
