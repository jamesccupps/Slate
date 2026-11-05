## Slate 0.16.1

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Fixes from a review of all of Slate's code.

### Windows and Linux
- If part of a big file couldn't be read while you edited it (a network drive that dropped for a moment), saving now asks first: an edit made since may have copied the blank bytes that read gave, and they would have been saved without a word.
- A project's `.editorconfig` can no longer freeze Slate. One with many `{a,b}` patterns took longer than forever to match; now any pattern is quick. `[*{1..3}.log]` and `[lib/**/*.js]` match the files they should, and EditorConfig is read beside the file instead of in the window, so a folder on a network drive that stopped answering doesn't hang opening or saving.
- Find and Replace with a regular expression: `\n` finds a whole line break in Windows-style (CRLF) files, so replacing `\n+` with `\n` or joining lines no longer leaves stray carriage returns.
- Format → Sort by this column works on a column that mixes dates with other text (it could stop with an error).
- Check and Format of an XML file whose tags have thousands of attributes take a moment instead of minutes.
- Bash's here-strings (`<<<`) no longer color the rest of a script as text.

### Windows
- A tab put back with unsaved changes no longer says another program changed its file when none did.
- The current search match is easy to read in the dark theme (its text is dark on the orange).
- If the graphics card resets while the window is being resized, Slate draws again instead of freezing.
- Saving a file only some people may read keeps it that way while it's being written, not only once it's done.
- Updates are checked against the version inside the downloaded file too, so a release can't hand out an older Slate under a newer number.
- Slate's settings folder and registry keys move to their new names only in a start that can't be undone (not right after an update, which goes back to the old version if the new one fails to start).
- Uninstalling, and Help → Stop opening files with Slate, leave alone registry keys named "Slate" that another program made.

### Linux
- A file changed by another program while Slate was closed (a `git pull`, a sync) is noticed when its unsaved tab comes back, instead of being overwritten by the next save.
- Closing the terminal Slate was started from, or Ctrl+C in it, asks about unsaved work Slate can't keep instead of quitting.
- Saving a file that belongs to someone else (shared through a group or an ACL), or to a group you aren't in, asks first: Slate saves by putting a new file in its place, which would be yours.
- Slate's data folder (`~/.local/share/slate`), which holds unsaved text and text piped in with `slate -`, can only be read by you.
