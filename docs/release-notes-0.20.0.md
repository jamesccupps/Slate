## Slate 0.20.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Nine things users asked for (issues #7 to #15), on Windows and Linux.

### New
- **Find in all tabs** (Edit menu): every line the find bar's search matches in every open tab, listed in a tab of its own; Enter or a double-click on a line goes there (issue #15).
- **Find in files** (Edit menu, Ctrl+Shift+F): the same over the text files in the folder of the tab's file and the folders in it, on another thread (Esc stops it). Version control, dependency and build folders, binary files and files over 32 MB are passed over (issue #15).
- **Replace in all tabs** (Edit menu): the find bar's search replaced in every tab that can be edited; each tab's change is one undo step there (issue #15).
- **Go back and Go forward** (Edit menu, Alt+Left and Alt+Right): to where the cursor jumped from, in any tab, as a browser's Back does (issue #11).
- **Files reopen where you left them**: a file opened again starts where its cursor and scroll were when you closed it (Settings → Reopen files where you left them, on by default; turning it off forgets them) (issue #11).
- **F7: the next word spelled wrong**, selected with its suggestions open; the spelling menu has **Change every “word” to**, which corrects every one of them at once (issue #12).
- **Open selected file or link** (File menu, and the text's right-click menu): a file named in the text opens in a tab (a relative name is in the folder of the tab's file, `name:12` goes to line 12), an http or https link opens in your browser. Nothing else is opened, and never a program. Give it a key in the shortcuts file (issue #7).
- **Your own file endings**: Format → Language → **Always open .mmd files as Markdown** (for whatever ending and language the tab has), kept in settings.json as `"languages"` (issue #13).
- **Interface text size** (Settings menu): Slate's own tabs, menu bar, find bar and status bar at 100% to 200%. By default they follow Windows' Text size (Settings → Accessibility); on Linux, the desktop's text scaling (issue #8).
- **Hide the menu bar** (Settings menu): it comes back while Alt (or Alt and a menu's letter) opens its menus (issue #10).
- **Find bar at the bottom** (Settings → Find bar → At the bottom): the find bar and a tab's notice sit just above the status bar, so opening them doesn't move the text (issue #9).
- **A Settings menu** (Alt+S), between Format and Help: how Slate looks and works (line numbers, tabs in the title bar, the menu bar, the find bar, font, theme, interface size, sessions, updates, keyboard shortcuts, default app, the settings folder). View keeps what the text shows (issue #14).

### Changed
- A file that can't be opened leaves you on the tab you were on, not the one beside it.
