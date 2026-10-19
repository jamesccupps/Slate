## Slate 0.12.4

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Two fixes for Windows.

### Windows
- A tab of a file you opened from inside a zip no longer waits for ever after Slate starts again. Explorer unpacks such a file into a temporary folder and deletes that folder later; Slate took the missing folder for a network drive that might come back, and the tab said "doesn't answer (a network drive?)". A folder that's gone from one of the PC's own disks now counts as gone: the tab closes, and the status bar says the file isn't there any more (unsaved text in such a tab comes back in a tab of its own, as for any file that's gone). Network drives, USB sticks and drives that aren't plugged in are still waited for.
- Help → Open files with Slate… says how Windows does it: only you can choose the app for a kind of file, and Windows asks the first time you open one (choose Slate and "Always"); its Default apps page may show Slate for a kind of file before you've chosen it. After setting up, a note above the text says which of the usual kinds of files (.txt, .log, .json, .xml, .md, .csv…) open with Slate, which Windows still asks about, and which another app opens.

On Linux nothing changed but the version.
