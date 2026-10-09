## Slate 0.8.2

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### Windows
- After *Help → Open files with Slate…*, Windows could ask "How do you want to open this file?" for a `.bat` file, offering Slate but not the way it ran before, and choosing Slate made `.bat` files open in Slate instead of running. Slate no longer registers for files Windows runs when they're double-clicked (`.bat`, `.cmd`, `.vbs`, `.js`, `.reg`, `.py`, `.rb`, `.pl`, `.ahk`, `.sh`) or for `.ts`, which is often a video. When 0.8.2 starts it takes that registration back by itself, along with a choice of Slate for `.bat` and `.cmd` files, so they run again. *Edit with Slate* on the right-click menu still opens them in Slate.
- New: *Help → Stop opening files with Slate…* (once Slate is set up). Slate comes off "Open with", the right-click menu and Default apps, and the file types you set to open with Slate go back to Windows' own choice. Slate stays installed, with your settings and tabs. Uninstalling does the same.

### Linux
- New: *Help → Stop opening files with Slate…*: the kinds of files Slate opens by default go back to the system's own choice.
