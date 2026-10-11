## Slate 0.11.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New: show only the lines that matter
- *View → Show only matching lines* (Alt+L) takes what's in the find box and opens the lines that have it in a tab of their own, numbered as they are in the file. Enter or a double-click on one goes to that line in the file. *Show lines not matching* does the opposite, to hide noise.
- It's quick on big files: the 809,820 lines with "ERROR" out of a 400 MB log in 0.3 s.
- Search the lines again in that tab to narrow them down further; the numbers stay those of the file.

### New: bookmarks
- Ctrl+F2 bookmarks the line the cursor is on (and takes the bookmark off again); F2 and Shift+F2 go to the next and previous one. Bookmarks show at the left edge of the text, and on Windows on the scrollbar too. They come back with your tabs.
- *Edit → Bookmarks* has them all, and *Clear bookmarks*.

### New: read-only tabs
- *File → Read only* stops a tab's text from being changed by accident; a lock shows on the tab, and "Read only" in the status bar (a click there makes it editable again).
- A file you can't write opens read only by itself.
