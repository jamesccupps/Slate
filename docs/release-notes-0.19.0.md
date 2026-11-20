## Slate 0.19.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New, on Windows and Linux
- **Multiple cursors** (issue #5), off unless you turn on **Edit → Multiple cursors → Use multiple cursors**. Then Ctrl+click adds a cursor or takes one away, and the same menu adds one above or below (Ctrl+Alt+Up / Ctrl+Alt+Down), the next match of what's selected (Ctrl+Shift+N; with nothing selected, the word at the caret first), every match (Ctrl+Shift+A), or one at the start or end of every selected line (Alt+[ / Alt+]); Skip to next match and Remove last cursor are there too. Typing, Backspace, Delete, Enter, Tab, the arrows, Home and End, Copy, Cut and Paste work at every cursor (a pasted text with as many lines as there are cursors puts one at each), each edit one step to undo; Esc goes back to one. With one cursor, nothing about Slate changes, and up to 10,000 can be made, so a huge file stays as quick as ever.
- **View → Find bar → Close when you click in the text**: the find bar closes as soon as you go back to the text (issue #6). Off unless you turn it on.
- **View → Find bar → One for each tab**: each tab keeps its own find bar open or closed, and a new tab starts without one; the search itself is shared, so F3 still finds it in any tab. Off unless you turn it on.

- **Help → Change keyboard shortcuts…**: give any command the keys you want. It opens a short file listing every command with its keys; take the `#` off a line, write other keys (`duplicate-line = Ctrl+Shift+D`, or nothing for none) and save, and Slate uses them at once, its menus included. A line Slate can't use is pointed out when you save.
- Linux: **Edit → Move line up / Move line down** in the menu (Alt+Up / Alt+Down, as before).

### Fixes, on Windows and Linux
- With the find bar open, typing in the text no longer makes its count ("3 of 12", or a red "No results") blink at every key: it changes only when the number does (issue #6). On Linux the matches marked in the text no longer blink either.
