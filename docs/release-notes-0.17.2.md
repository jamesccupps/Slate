## Slate 0.17.2

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Fixes from an outside review of Slate's code, mostly small.

### Windows and Linux
- A column selection (Alt+drag) in a file with a very long line no longer makes the window stutter: Slate reads the line only as far as the selection's columns, instead of all of it at every move of the mouse and every repaint. A column selection reaches out to column 100,000.
- Dragging a selection of more than 16 MB somewhere else says it's too much text, as Duplicate does, instead of copying all of it into memory (a press in a big selection that slipped a little was enough).
- `slate --version` and `slate --help` answer (on Windows, in the terminal Slate was started from), and `--` ends the switches, so a file named like one opens: `slate -- --notes.txt`.
- `some-command | slate -` no longer holds all of what's piped in in memory before handing it to Slate; text bigger than 64 MB opens like any big file instead of being read into memory all at once.
- A thread that can't be started (the system short of memory) fails what it was for with a message instead of ending Slate, and a crash in one part of Slate can no longer leave the window failing at every repaint.
- Updates are only downloaded from Slate's own release page.

### Linux
- Closing the rightmost tab with unsaved changes and answering "Save" no longer ends in "Something went wrong" (the file was saved, but quitting stopped there).
- Opening a pipe or a device (`/dev/zero`, a FIFO) says it isn't a regular file, instead of a tab that stayed on "Opening…" for ever; saving over one is refused too.
- `slate --wait` (git's `core.editor`) and `slate -` no longer print a GLib warning in the terminal, and neither does Shift+Alt+F (Format).
- An unknown switch says so and lists where to look, instead of GTK's own help.
- Without a home folder (`env -i`, some services), Slate's data goes in a folder of the user's own (`/tmp/slate-<uid>`), never one another user could have made first.
- Removing the package leaves alone a `slate.list` it didn't add.

### Windows
- If Windows' Direct2D can't be used, Slate says so instead of ending without a word.
