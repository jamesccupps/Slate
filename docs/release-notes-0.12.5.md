## Slate 0.12.5

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Small fixes from a review of the whole program.

### Windows
- Save as… for a new document, and Open…, start in the folder you last opened or saved a file in (or in Documents), not in Slate's own program folder.
- When there are more tabs than fit, the tab strip shows whole tabs: none is cut off at the edge. Scrolling over the tabs moves one tab per notch of the mouse wheel.
- Copy file path and Copy JSON / XML path say so when another program is holding the clipboard, as Copy does, instead of saying it was copied.

### Windows and Linux
- Help → Keyboard shortcuts takes the place of an empty new tab you haven't typed in, as opening a file does.

### Linux
- A file that can't be opened no longer switches the window to another tab.
- In a narrow window, the status bar gives up the indentation, the line endings and the language first, so the line and column stay readable (as on Windows); a message there (such as "Zoom 125%") goes when you switch tabs or open a file.
- The find bar's Aa, ab and .* buttons are narrower, so the search box keeps its room in a narrow window.
