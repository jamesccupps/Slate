## Slate 0.10.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

### New: Print
- *File → Print…* (Ctrl+P) prints the file, or only the selection, in its syntax colors, with line numbers when they're shown, the file's name at the top of each page and the page's number at the bottom. A long line goes on in the next row, breaking between words.
- The system's print dialog picks the printer, the pages and the number of copies. *Microsoft Print to PDF* on Windows and *Print to File* on Linux make a PDF.
- Slate asks first before printing more than 100 pages, and prints up to 10,000 at a time (select the part you need of a bigger file, then print the selection).
