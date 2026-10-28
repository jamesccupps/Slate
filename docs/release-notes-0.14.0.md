## Slate 0.14.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Print preview and page setup, column selection, dragging text, highlighted searches, Find through all open tabs, a tab of matching lines that follows a log, and big files faster on several cores.

### Printing
- **File → Print preview…**: the pages as they'll print, laid out for your printer. On Windows in a window of its own: page by page (the arrows, Page Up / Page Down), the whole page or its width, and Print, which goes on to the print dialog. On Linux, GTK's preview.
- **Options:** Colors (off: black text, bold and italic kept), Line numbers, and Header and footer (the file's name and the page's number). On Windows in the preview's bar, on Linux in the print dialog's Text tab. They're kept for next time and Print follows them. Line numbers on printouts are an option of their own now, no longer the editor's.
- **File → Page setup…**: paper, orientation and margins (Windows' own dialog, also in the preview's bar; on Linux GTK's, with the margins in the print dialog's Text tab).

### Editing
- **Column selection:** Alt+drag, or Shift+Alt+arrows, selects a block of columns over several lines. Typing, Paste, Backspace and Delete then work on every line (a column you copied pastes one line into each), and Copy and Cut take each line's part. Shift+Alt+Up/Down no longer move the line (Alt+Up/Down still do).
- **Drag and drop of text:** drag the selection to another place to move it, or hold Ctrl to copy it. It's one step to undo.

### Find
- **Highlight** (the new button in the find box, or Alt+H): the search stays highlighted in every tab, each search in a color of its own. Highlight it again to take it away, or use View → Clear highlights. With highlights kept, Show only matching lines keeps the lines that have any of them.
- **All tabs** (the new button in the find box, or Alt+A): Find next and previous go on through the other open tabs, and the tab with the next match comes up ("Found in notes.txt").

### Matching lines
- A tab of matching lines **follows its file**: when the file is read again (a log that grew), the tab is made again, at its end if you were there. A tab of lines filtered twice keeps both searches.

### Faster on several cores
- **Count all** (the "3 of 120" in the find bar) searches a big file in parts side by side: 850 MB of JSON in 0.18 s instead of 0.39 s, "ERROR" in a 400 MB log in 0.13 s instead of 0.23 s. Regular expressions too (about a third less time).
- **Show only matching lines**: the lines with "ERROR" out of a 400 MB log in 0.23 s instead of 0.29 s.
- **Sort lines**: 4.85 million lines in 2.0 s instead of 2.8 s. **Sort by this column** (CSV): 5.2 million rows in 2.1 s instead of 3.2 s.
- **Remove duplicate lines**: 4.85 million lines in 0.47 s instead of 1.0 s.

### JSON and XML structure panel
- An open array element stays open when elements are added before it. It used to close, and the one now at its old place opened instead.
- While the panel catches up after an edit, a click on a row goes to that value where it is now. It used to ask you to try again.
