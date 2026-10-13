## Slate 0.12.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New: screen readers (Windows)
- Narrator, NVDA and JAWS can read the text: the line, word or character at the cursor as it moves (with word wrap on, the line as it shows), the selection, and what changes as you type. Switching tabs says the tab's name.
- Reading stays quick on huge files: a screen reader asking for all of an 800 MB file gets its first million characters, and moving to the end takes a fraction of a second.
- Not yet: the tab strip, the status bar and the menu bar (the menus themselves are Windows' own and are read), and Slate's messages ("Saved", "Not found"). On Linux, Orca can't read the text yet.

### Security
- Slate.exe loads the Windows DLLs it needs from Windows' own folder only, never from the folder Slate.exe is in. Run from Downloads, it can't pick up a DLL that a web page got saved there.
- Saving keeps a file's own permissions in every case. A private file moved into a shared folder could get that folder's permissions after a save, so others could read it.
- An encrypted file in a folder that can't hold encrypted files isn't saved there (it would end up unencrypted); the message now says so.
- The syntax colors, JSON and XML tools and the structure panel were checked against 14,000 malformed files: none made Slate crash or hang.
- None of this makes Slate slower: startup, opening and saving take the same time as in 0.11.0.
