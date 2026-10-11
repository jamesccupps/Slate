## Slate 0.9.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

This release fixes what an outside review of both versions found, checked one finding at a time.

### Faster
- Searching a JSON file for a key and its value, like `"status":"refunded"`, is 3 to 5 times faster (0.4 s instead of 1.4 s in an 850 MB file).
- Sorting the lines of a big file is nearly 3 times faster (it uses all the processor's cores), and the line tools need less memory.
- Typing in a JSON or XML file of a few MB with the path bar on no longer reads the whole file again after every key: the path bar catches up as soon as you pause.

### Safer
- A file nested absurdly deep (thousands of `[` or `<a>` inside each other) no longer makes Slate use gigabytes of memory, which could end it. Formatting one that would come out absurdly big is refused instead of filling the memory or the disk.
- Saving keeps more of a file as it was. On Windows that's its own permissions (not just its folder's), its encryption, and its alternate data streams, such as the mark that it was downloaded from the internet. On Linux it's its extended attributes (ACLs, SELinux labels), and the new file can only be read by you while it's being written.
- When another program rewrites part of a big open file without changing its size, Slate now notices, so saving can't mix that program's changes with yours.
- Saving as UTF-16 asks first when some bytes that aren't text can't be kept.
- Sorting no longer drops a lone carriage return at the very end of a file.

### Windows
- `slate file.txt:120:5` (a line and a column, as compilers print them) opens the file there. It said the name was invalid.
- Uninstalling now removes `Slate.exe` too, including when winget uninstalls it.
- "Saved" no longer stays in the status bar after you start typing again.
- Reload asks about the tab you asked to reload, and reloads that one, even if another tab came to the front meanwhile.

### Linux
- Fixed: opening a folder, or a file that couldn't be opened, could close Slate and lose your latest typing.
- Fixed: unsaved tabs could be lost when Slate couldn't read its session file (after going back to an older version, say) or couldn't write it (a full SD card). The session now works as on Windows. Copies of unsaved text are never deleted because the list of tabs couldn't be read, and copies no tab names come back as tabs of their own. When Slate can't keep your work, closing asks about every unsaved tab. The session is also written in the background now, so typing doesn't pause.
- Logging out, shutting down or closing the terminal Slate runs in now saves your tabs first.
- `slate notes.txt:120` from a terminal opens the file at that line, and files whose names aren't UTF-8 open.
- A file another program writes to (a log) stays where you were when Slate reads it again, and follows the end if you were at the end.
- The menus and shortcuts now match Windows':
  - *Line endings* changes every line break in the text.
  - *Toggle comment* uses `;` in `.ini` files.
  - The Format menu shows the JSON or XML items only for those files, and *Minify XML* is new.
  - *File → Open recent* is new.
  - Ctrl+1 … Ctrl+9 go to a tab (Alt+1 … Alt+9 still do), and Shift+Alt+F formats XML too.
- After *Save as ANSI anyway*, the tab now stays open. A save that fails shows a note above the text with a *Save as…* button.
- *Stop opening files with Slate…* leaves your choices for other apps alone.
