## Slate 0.19.2

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Fixes, most of them found by a review of 0.19.0's new features. (There was no 0.19.1: its fixes are in this one.)

### Windows and Linux
- With **View → Find bar → Close when you click in the text** on, a click in the text puts the cursor where you clicked again (issue #6). The bar closed at the press, so the text moved up under the mouse, and the click (or a double-click right after it) landed on another line, or the drag from it selected a few. Now the bar waits until your clicks are over and closes a moment after, so a click, a double-click, a triple-click or a drag lands where you made it.
- **Save** looks at the file once more just before writing it. If another program changed it a moment before (sooner than Slate's own look every 2 seconds would have noticed), nothing is saved and Slate says so; save again to keep your version, or reload to see the other one.
- **Multiple cursors:** Home, End, Up and Down move every cursor the same way. Home stops where a line's indentation ends, as it does with one cursor, so "cursors on these lines, Home, type" types at the same place on each; with word wrap on, Up and Down go by lines at every cursor.
- **Multiple cursors:** Ctrl+double-click adds the word as one more selection (Ctrl+triple-click the line) instead of dropping the other cursors, and Ctrl+Up / Ctrl+Down scroll without dropping them.
- **Keyboard shortcuts** file: the comma key can be written (`Ctrl+,`). A line with a mistake no longer takes its command's keys away: it changes nothing, and the message says that command keeps the keys it had. A file saved with a byte order mark no longer loses its first line.
- A regex that can match nothing (`x*`) no longer finds a match between the two characters of a Windows line break, so Replace all can't split one.
- Move line up / down in a file with both kinds of line break keeps the selection on the moved lines (it reached a character into the next line, and Delete line then took three lines).
- **EditorConfig** applies to new files too: a file named when starting Slate that isn't there yet (Windows), and a new tab saved into a folder.
- No spell check in a file Slate opened as binary.

### Windows
- With **One find bar for each tab**, switching tabs no longer leaves the keyboard in a box that isn't shown.
- In the keyboard shortcuts file, `Ctrl+?` works (it's Shift and the `/` key), and Alt with a menu's letter (`Alt+F`) given to a command is that command's: the menu still opens with Alt, then the letter, or F10.
- "Valid JSON" no longer stays in the status bar beside a notice saying the text isn't valid.
- Print preview, Clear highlights and Theme have menu letters of their own (W, G, E).

### Linux
- The Right and Left arrows in an open menu go to the menu beside it. On GNOME (Wayland) the next menu didn't show, and from then on the menus, and clicks in the text, stopped working until Slate was started again.
- **File → Print** has its **Text** tab again: colors, line numbers, header and footer, margins.
- After Format, Enter and Tab indent by the formatted text's indentation.
- Unsaved tabs are numbered (Untitled 2, Untitled 3), as on Windows.
- Ctrl+Up and Ctrl+Down scroll the text by a line again when that takes the cursor off the screen, as on Windows.
