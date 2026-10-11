## Slate 0.9.1

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### Faster
- Counting all matches and Replace all in big files are about 10% faster than in 0.8.2. 0.9.0 had made them about 5% slower: it cleared each piece of the file before reading it.
- Searching while ignoring case for text like `Wool socks` is back to full speed: all 435,571 matches in an 810 MB file in 0.28 s (0.37 s in 0.9.0, 0.31 s in 0.8.2). 0.9.0 made searches like `"status":"refunded"` 3 to 5 times faster but this kind about 20% slower; Slate now tries both ways at the start of the file and keeps the faster one.

### Windows
- Updates check who signed the new `Slate.exe` more closely: the identity the signing service verified, not just the name.
- A file you open while Slate is running goes only to a Slate running as you, with the same rights.

### Linux
- Slate's menu commands are no longer published on the session bus (D-Bus).
- The apt repository's index is valid for a year from signing and is signed again every month, so an old copy of it can't pass for the current one.
