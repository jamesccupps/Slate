## Slate 0.19.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New, on Windows and Linux
- **View → Find bar → Close when you click in the text**: the find bar closes as soon as you go back to the text (issue #6). Off unless you turn it on.
- **View → Find bar → One for each tab**: each tab keeps its own find bar open or closed, and a new tab starts without one; the search itself is shared, so F3 still finds it in any tab. Off unless you turn it on.

### Fixes, on Windows and Linux
- With the find bar open, typing in the text no longer makes its count ("3 of 12", or a red "No results") blink at every key: it changes only when the number does (issue #6). On Linux the matches marked in the text no longer blink either.
