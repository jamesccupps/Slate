## Slate 0.12.1

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

A polish release: every screen was looked at again (light, dark and high contrast, 100% to 200%, windows down to the smallest size), and what was off is fixed.

### Windows
- With many tabs open, making the window narrower could scroll the tab you're on out of view. It stays in view now.
- The accent color is the shade Windows 11's own apps use on each theme: a lighter one in dark mode, where the plain accent was hard to see (bookmarks, the note above the text, "Update to …" in the status bar, the Go button).
- In a narrow window, the status bar's word and character counts make way for a message, so it isn't cut short.
- "Checking for updates…" no longer stays beside "Update to …", and "Still reading the file's lines…" and "Still opening the file…" go away as soon as that's done.
- A tab whose file is still opening (a slow network drive) shows "Opening…" on its first line, without a caret or the size and line endings of an empty document.
- The XML structure panel counts from 1 (`[1 … 100]`), as XML paths do (`book[1]` is the first).
- The structure panel takes at most half of a narrow window, so the text keeps room for its lines.
- A regular expression with a mistake says what it is in plain words ("Unclosed group").
- The Keyboard shortcuts tab is read only, so a key typed there by mistake doesn't leave an unsaved tab.

### Linux
- A message in the status bar ("Saved", "Zoom 125%"…) goes away after a few seconds, as on Windows. Until now the line and column didn't come back until another message replaced it.
- With the find bar open, the matches are counted and marked in whichever tab you switch to, a tab of matching lines included.
- Check and Format say an invalid JSON or XML above the text ("Not valid JSON: … (line 3, column 14)"), as on Windows, until it's fixed.

None of this makes Slate slower: startup takes the same time as in 0.12.0.
