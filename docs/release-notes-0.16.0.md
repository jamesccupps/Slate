## Slate 0.16.0

**Download:** on Windows, `Slate.exe` below (64-bit Windows 10 or 11, signed); Slate 0.2.0 or newer offers this update in its status bar by itself. On Linux, Slate installed from its package updates with `sudo apt upgrade`; to install it, see the [README](https://github.com/jamesccupps/Slate#linux-and-raspberry-pi).

Linux catches up with Windows, and a few new tools for both.

### Linux
- The **JSON and XML path bar and structure panel**: where the caret is (`data › [1203] › name`), click a part to go there; the panel (Ctrl+Shift+O) browses objects, arrays and elements, huge files too, and follows the caret.
- **Show whitespace**, the **matching bracket** highlighted, **overtype** (Insert; OVR in the status bar).
- The status bar counts **words and characters** (of the selection too, with its lines), and its indentation and encoding open their menus.
- The **Encoding** (save with, reopen as), **Indentation**, **Font** and **Formatting indent** menus.
- File → **Reload from disk**, **Show in folder**, **Copy file path**, **Close other / saved / all tabs**, **Restore last session**; Open recent → **Clear list**.
- A right-click on a tab opens **its menu**; when the tabs don't fit, a button **lists them all**.
- Help → **Open settings folder** and **Report a problem…**.
- The **sun/moon button** at the end of the menu bar switches light and dark, and with *Use system setting* Slate follows the desktop's light or dark style **as it changes** (GNOME, and desktops that have the portal's setting).
- The text's menu has the document's own tools: Format and Check for JSON and XML, Copy JSON path / XPath, and CSV's sort.

### Both
- **Format → Convert**: Base64, URL encoding and JSON string escapes, each way, on the selection (or the whole text); and the **SHA-256** of the selection or the file, with Copy.
- **EditorConfig**: a project's `.editorconfig` sets the indentation of its files, the line endings and encoding of new ones, and a line break at the end when they're saved.
- **Go to line** (Ctrl+G) takes a **JSON or XML path** too: `data[1203].name`, `["my key"][0]`, `/catalog/book[3]/title`.

### Windows
- When Windows restarts for an update, Slate **opens again with its tabs**, as Notepad does (with *Restart apps* on in Windows' sign-in options).
- Slate's settings folder is now `%LOCALAPPDATA%\JamesCupps\Slate`, and its registry keys are named for its publisher too, so another program called Slate can't share them. Slate moves them from the old names (`%LOCALAPPDATA%\Slate`) by itself the first time it starts; the file types you set to open with Slate stay as they are.
