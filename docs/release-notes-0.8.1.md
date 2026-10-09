## Slate 0.8.1

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux (64-bit, with GTK 4.8 or newer: Debian 12, Raspberry Pi OS 12 (Bookworm) on a Raspberry Pi 4 or 5, Ubuntu 23.04 and newer), Slate installed from its package updates with `sudo apt upgrade`; to install it, run these two lines in a terminal:

```
wget -O /tmp/slate.deb https://github.com/jamesccupps/Slate/releases/latest/download/slate-linux-$(dpkg --print-architecture).deb
sudo apt install /tmp/slate.deb
```

### Windows
- Updates also check that the new `Slate.exe` is signed by the same publisher as the one running (James Cupps), on top of the release's checksum.
- `Slate.exe --install` sets Slate up for your account without asking anything (what Help → Open files with Slate… does, without opening Default apps): for winget and scripts. Installed apps now shows James Cupps as the publisher.

### Linux
- The View and Format menus show what's on: check marks at Word wrap, Line numbers, the theme and the tab's line breaks, and a dot at the tab's language.
- Go to line leaves the view where it is when the line is already on screen.
