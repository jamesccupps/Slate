## Slate 0.10.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New: Print
- *File → Print…* (Ctrl+P) prints the file, or only the selection, in its syntax colors, with line numbers when they're shown, the file's name at the top of each page and the page's number at the bottom. A long line goes on in the next row, breaking between words.
- The system's print dialog picks the printer, the pages and the number of copies. *Microsoft Print to PDF* on Windows and *Print to File* on Linux make a PDF.
- Slate asks first before printing more than 100 pages, and prints up to 10,000 at a time (select the part you need of a bigger file, then print the selection).

### Fixed
- Changing the line endings keeps the cursor on its line. On Linux it could end up inside a line break, and the next key then left a stray character in the file.
- Saving as UTF-16 a file with bytes that aren't text (a mostly UTF-8 file) asks about UTF-16. It asked about ANSI, and "Save as ANSI anyway" saved in ANSI, turning characters such as Japanese into "?".
- Format on JSON or XML whose result would be far too big says what's wrong with the text when it isn't valid (2 MB of `[`, say), instead of only that the result would be too big.
- Check and Format stop at nesting more than 50,000 levels deep, which no real file has, instead of using memory for every level.
- Find no longer risks holding the window up for a moment on an unusual search in an unusual file (a query like `,000…0` in a file of zeros) while it works out the fastest way to search.

### Windows
- The path bar and the structure panel work out a pretty-printed JSON file's structure about a third faster (0.22 s instead of 0.33 s for 103 MB).
- The scrollbar's marks follow a new search that has as many matches as the one before.
- While you type in a big JSON or XML file, its structure isn't read again in the background until you pause.
- Slate started minimized and closed without being shown no longer sets the last session's tabs aside at the next start, as if it had crashed.
- Uninstalling never deletes a Slate installed again right after (`winget uninstall`, then `winget install`, say).
- A file with permissions of its own, encryption or the downloaded-from-the-internet mark is still saved when Windows can't keep those for it (a big file Slate still reads from, say); before, saving could fail.
- Updates compare only the signing identity, not also the signing service's own marks, and a download signed under another identity says to download the new version yourself.

### Linux
- Logging out with unsaved tabs when Slate can't write its session (a full disk) now waits and asks about them, instead of losing them.
- After Save As, the original file opens again in a tab of its own (before, the tab that was saved under the new name was shown).
- A tab that's still saving closes once it's saved (closing it and choosing Don't save stopped the save). The window's close button doesn't ask a second time while a question is open.
- A file that can't be read again after another program changed it is no longer retried, with a red message, every 2 seconds.
- `slate notes.txt` in a folder reached through a symbolic link keeps that folder's name, as the shell shows it.
- The keyboard shortcuts tab saved as a file is named after its file; a transform finished in a tab that isn't shown is kept in the session right away.
- Saving a file whose owner Slate can't keep doesn't keep setuid or setgid (as `cp -p`); extended attributes are kept however many there are.
