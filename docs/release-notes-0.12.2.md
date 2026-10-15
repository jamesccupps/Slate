## Slate 0.12.2

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

The fixes from another audit of Slate, its packages and its website.

### Windows and Linux
- Check is as strict as JSON and XML are. A JSON file has to be UTF-8 text, and a file of several values has to have one per line (JSON Lines). An XML file can only have the characters XML allows, character references to them (`&#1;` isn't one), and its declaration (`<?xml …?>`) only at the very start. Format still formats such files: it's Check that says what's wrong.
- Saving a file you aren't allowed to change (another user's, on Linux) says so and leaves the file alone, instead of replacing it with a copy of your own. In a shared folder such as `/tmp`, where only a file's owner can replace it, the message says that, not that the file is in use.
- Sessions set aside after a crash are deleted 30 days later, as copies of damaged files already were.

### Windows
- A file whose name isn't valid Unicode (Windows allows a few such names) opens from the command line, from Explorer and by drag and drop, and comes back as a tab at the next start.
- If Windows can't check the running Slate's signature on a PC, Slate doesn't install updates by itself there: it says where to download the new version.
- Uninstalling removes the installed copy whatever its folder is called.

### Linux
- Slate's apt repository no longer has an expiry date, so `apt update` never stops trusting it because a monthly renewal didn't happen.
- The install command in the README and on the website lets apt read the downloaded package, so it doesn't warn about it, and says that 32-bit Raspberry Pi OS (armhf) isn't supported.
- The package has the files Debian's tools look for: checksums (`dpkg --verify`), the license in Debian's format and a changelog. Slate is listed once in the app menu, under Accessories, and installing it doesn't add a second entry for its repository when the system has one already.

None of this makes Slate slower: Check takes the same time as in 0.12.1 (an 826 MB JSON file, a 197 MB XML file), as the characters are checked on a second core while the file is read, and opening, searching, Show only matching lines and saving take the same time as before (a 400 MB CSV, a 400 MB log, 300 MB of plain text).
