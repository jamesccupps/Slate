## Slate 0.15.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Spell check, input methods' text in place, and the tabs in the title bar.

### Spell check
- Words spelled wrong get a **wavy red line**, as in Notepad: in plain text, in Markdown's prose (not its code or links) and in comments in code. Names (`camelCase`, `snake_case`, `NASA`), links, e-mail addresses, file names, numbers and data files (JSON, CSV, logs) are left alone, and the word you're typing waits until you've moved past it.
- **Right-click** a word with the line for what it may have been meant to be, **Add to dictionary** or **Ignore** (until Slate closes).
- It's the system's spell checker, in its language: on Windows, Windows' own (Add to dictionary goes into the dictionary Windows keeps for all programs); on Linux, Enchant with Hunspell's dictionaries, which Slate's package now recommends (without them, nothing is marked). Only the words on screen are checked, so a file's size doesn't matter.
- **View → Spell check** turns it off.

### Input methods
- For Chinese, Japanese and Korean, what's being composed now shows **in the text at the cursor**, pushing what's after it aside, in Slate's font and colors (on Windows it used to be Windows' own little window over the text, light even in the dark theme). Its clauses are underlined, the one being converted with a thicker line, and the list of candidates opens under it.
- Clicking elsewhere while composing keeps what was composed, where it was.

### Tabs in the title bar
- The tabs are **in the title bar**, as in Windows 11's Notepad, which leaves more room for the text. On Windows, Slate draws the window's minimize, maximize and close buttons; Snap layouts still open over maximize, the empty part of the tab strip moves the window (double-click to maximize, right-click for the window menu) and the top edge resizes it. With Windows' high contrast on, Windows' own title bar comes back. On Linux, the tabs and the desktop's own window buttons are the window's title bar.
- **View → Tabs in the title bar** puts them back under the title bar (on Linux, from the next start).
