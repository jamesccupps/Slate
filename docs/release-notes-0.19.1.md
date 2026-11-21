## Slate 0.19.1

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Two small fixes.

### Windows and Linux
- With **View → Find bar → Close when you click in the text** on, a click in the text puts the cursor where you clicked again (issue #6). The bar closed at the press, so the text moved up under the mouse while the button was still down, and the drag from the press could select a few lines. Now the find bar (and on Windows the Go to line bar) closes when you let the button go, and the text scrolls back by the bar's height, so it stays where it was on screen: a click, a double-click, a triple-click or a drag lands where you made it.

### Linux
- Ctrl+Up and Ctrl+Down scroll the text by a line again when that takes the cursor off the screen, as on Windows. The view went back to the cursor at once, so they seemed to do nothing.
