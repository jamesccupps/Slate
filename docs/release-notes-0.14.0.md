## Slate 0.14.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Big files go faster on several cores, Find goes through all open tabs, a tab of matching lines follows a log, and the JSON structure panel keeps up with your edits.

### Faster on several cores
- **Count all** (the "3 of 120" in the find bar) searches a big file in parts side by side: 850 MB of JSON in 0.18 s instead of 0.39 s, "ERROR" in a 400 MB log in 0.13 s instead of 0.23 s. Regular expressions too (about a third less time).
- **Show only matching lines**: the lines with "ERROR" out of a 400 MB log in 0.23 s instead of 0.29 s.
- **Sort lines**: 4.85 million lines in 2.0 s instead of 2.8 s; **Sort by this column** (CSV) 5.2 million rows in 2.1 s instead of 3.2 s.
- **Remove duplicate lines**: 4.85 million lines in 0.47 s instead of 1.0 s.

### Find
- **All tabs** (the new button in the find box, or Alt+A): Find next and previous go on through the other open tabs, and the tab with the next match comes up ("Found in notes.txt").

### Matching lines
- A tab of matching lines **follows its file**: when the file is read again (a log that grew), the tab is made again, at its end if you were there. A tab of lines filtered twice keeps both searches.

### JSON and XML structure panel
- An open array element stays open when elements are added before it (it used to close, and the one now at its old place opened).
- While the panel catches up after an edit, a click on a row goes to that value where it is now (it used to ask you to try again).
