## Slate 0.4.0

Big files keep their unsaved changes now, network files no longer freeze the window, and there's a set of small editing features people expect, 13 more languages and an XML structure panel.

**Download:** `Slate.exe` below, for 64-bit Windows 10 or 11. If you have 0.2.0 or 0.3.0, it offers this update in its status bar by itself (*Help → Check for updates* asks right away).

### Your work
- Unsaved changes to files over 64 MB come back after closing Slate, like smaller files' already did. Only what you typed or pasted is kept, never a copy of the file. If the original file changed or is gone when you come back, your edits aren't laid over it: the text you added opens in a tab of its own, the file opens as it is now, and Slate tells you why.
- A tab whose file is on a network share that doesn't answer waits and tries again by itself (or when you click *Try now*), and opens once the share is back.
- Files open and reload in the background: a slow share or disk no longer freezes the window ("Opening…" shows meanwhile).

### Editing
- *View → Show whitespace*: spaces, tabs and line endings drawn faintly.
- The bracket next to the caret and its partner are highlighted.
- *Reopen closed tab* (Ctrl+Shift+T) brings back the last 20 closed files, at the same place.
- *Close saved tabs* and *Close all tabs* (they ask about unsaved changes as usual).
- Insert switches to overtype ("OVR" in the status bar).
- The status bar counts words and characters (of the selection, and of documents up to 64 MB), and shows the indentation in use: click it to change it.
- *File → Restore last session* and *Format → Formatting indent* are in the menus now.
- When the tabs don't fit, a button lists them all.
- `slate notes.txt:120` (or `:120:5`) opens at that line.

### Formats and colors
- 13 more languages: VBScript/VBA, AutoHotkey, TOML, nginx and Apache configuration, Perl, R, Terraform/HCL, CMake, `.properties`, subtitles (`.srt`, `.vtt`), calendars and contacts (`.ics`, `.vcf`) and Visual Studio solutions.
- Better detection by name, folder and content: `.conf` files are told apart (nginx, Apache, XML or INI), `CMakeLists.txt`, `Jenkinsfile`, `.htaccess`, `.env.local`, `*.yaml.j2`, and the `hosts` file.
- Code blocks in Markdown are colored in their language (```` ```rust ````, ```` ```json ```` …).
- XML gets a path bar (`catalog › book[3] › title`) and a structure panel like JSON's, fast on huge files; *Copy XML path* copies the XPath.
- C++ digit separators and raw strings, C# nested interpolation, indented Sass and YAML strings over several lines are colored right.

### Accessibility
- With Windows' high contrast on, Slate uses its colors.
- Magnifier and other tools that follow the text cursor can follow Slate's.

### Help
- If Slate ever crashes, it writes a crash log and a small crash dump in its settings folder, and nowhere else. *Help → Report a problem…* opens a GitHub issue form with the version filled in, for you to complete and send yourself.
- Slate is open source under the MIT license.

Checksum: `Slate.exe.sha256` (SHA-256 of `Slate.exe`).
