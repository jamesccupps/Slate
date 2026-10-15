## Slate 0.12.3

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Quicker to open, and lighter on big files.

### Windows
- The window opens sooner. Where the graphics card's driver is slow to load (a quarter of a second on an NVIDIA card), Slate no longer waits for it: the first frames are drawn without it, and the card takes over as soon as it's ready. On such a PC the window shows its text in under a tenth of a second instead of a quarter.
- No more white flash when the window opens: it comes on the screen with its first frame already drawn.

### Windows and Linux
- Sort lines, Remove duplicate lines, Remove blank lines and Trim line ends write their result as they go, instead of building all of it in memory first. On a 400 MB log of 4.85 million lines, Sort takes 2.8 s instead of 4.5 s and at most 490 MB of memory instead of 830 MB; Remove duplicate lines takes 1.1 s instead of 2.4 s.
- crash.log no longer grows without end: past 1 MB it starts over, keeping the one before as crash.log.old.

### Linux
- An idle Slate sleeps. The caret's timer stops once the caret has stopped blinking (it woke Slate twice a second for as long as it ran), and the look at the open files every 2 seconds redraws the window only when a file changed (it redrew it twice every 2 seconds). Less work for a Raspberry Pi or a laptop's battery.

None of this makes anything else slower: typing, scrolling and searching take the same time as in 0.12.2.
