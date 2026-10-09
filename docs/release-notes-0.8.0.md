## Slate 0.8.0

Slate now runs on Linux too, Raspberry Pi included.

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11); if you have 0.2.0 or newer, it offers this update in its status bar by itself. On Linux (64-bit, with GTK 4.8 or newer: Debian 12, Raspberry Pi OS 12 (Bookworm) on a Raspberry Pi 4 or 5, Ubuntu 23.04 and newer), run these two lines in a terminal; they get the package for your computer, and the same two lines install a newer version later:

```
wget -O /tmp/slate.deb https://github.com/jamesccupps/Slate/releases/latest/download/slate-linux-$(dpkg --print-architecture).deb
sudo apt install /tmp/slate.deb
```

Or download `slate-linux-arm64.deb` (64-bit ARM, like the Pi) or `slate-linux-amd64.deb` (PCs) below and install it with `sudo apt install ./slate-linux-arm64.deb`; the `.tar.gz` files are the same program to run without installing.

### Linux
- The same engine as on Windows: huge files open instantly (an 826 MB JSON file in a quarter of a second, its lines counted in another quarter), search, Replace all, JSON and XML format, minify and check, and the line tools work the same way, on other threads.
- The same 52 languages' colors and editing: tabs, find and replace, go to line, comments, moving and duplicating lines, word wrap (wrapped lines continue under their indentation), line numbers, zoom, dark and light (following the desktop).
- Tabs and their unsaved text come back after a restart; files changed by other programs are noticed and reloaded when they have no unsaved changes.
- Linux habits: one Slate per session (opening a file from the file manager or `slate notes.txt` in a terminal adds a tab), files dropped on the window open, selecting text makes it available to middle-click paste, new files get LF line breaks.
- Not on Linux yet: the JSON/XML structure panel and path bar, updating itself (install a newer `.deb`), show whitespace, bracket matching and overtype.

### Windows
- Nothing changes in how Slate works on Windows; under the hood, the engine, colors and editing are now shared with the Linux version.
- GitHub now builds and tests every release for Windows and for Linux on x86-64 and ARM64; only the step that drafts the release can write to the repository.
