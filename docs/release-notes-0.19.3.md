## Slate 0.19.3

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Two fixes for issues reported after 0.19.2.

### Windows and Linux
- **Undo with multiple cursors** puts every cursor back where it was (issue #5). Typing at several cursors and then pressing Ctrl+Z took the typing back but left only one cursor; now they all come back, so you can carry on typing at all of them. Redo brings them back as they were after the typing.

### Windows
- **Format → Language** is dark when Slate's menus are (issue #16). Its three columns made Windows draw it in light colors; now it lists Plain text first and the languages in groups by their first letter (A – C, D – I, …), with the group holding the current language showing its name. The status bar's language menu is the same.
