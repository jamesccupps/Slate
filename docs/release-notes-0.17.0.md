## Slate 0.17.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Linux, polished on a real desktop: Slate was checked on Ubuntu 24.04 with GNOME on Wayland, screen by screen, and timed against Windows on the same PC with the same files.

### Linux
- High contrast: when the desktop's high contrast is on (GNOME's Settings → Accessibility, or a high-contrast theme), Slate draws its text and tabs in black and white with no syntax colors, selected text in white on blue, and follows the switch while it runs.
- Switching GNOME from Dark back to Light now switches Slate too (it stayed dark until it was started again).
- Alt+C, Alt+W and Alt+R in the find box turn on Match case, Whole word and Regular expression, as on Windows.
- Text selected with the keyboard (Shift+arrows, Ctrl+A) or by Find can be pasted with a middle click in any program, as text selected with the mouse already could.
- No more "GLib-CRITICAL" warning in the terminal every time Slate starts on GNOME.

### Speed
- Measured with the same scripts and files on both systems (the same PC, four cores each): finding, counting lines and matches, sorting lines and CSV rows, uppercase, Undo, saving, checking, formatting and minifying JSON, and checking and formatting XML take the same time on Linux as on Windows, or less.
