## Slate 0.5.0

Slate now reads PPCL, the language of Siemens APOGEE and Desigo field panels, and its prompts follow dark mode.

**Download:** `Slate.exe` below, for 64-bit Windows 10 or 11. If you have 0.2.0 or newer, it offers this update in its status bar by itself (*Help → Check for updates* asks right away).

### PPCL
- Programs are colored much as Desigo's own editor shows them: commands and `IF`, `THEN`, `ELSE`, `GOTO` in the keyword color, point names in quotes, states and resident points (`ON`, `OFF`, `FAILED`, `DAY`, `TIME`, `$LOC1`, `@OPER`) in a color of their own, `C` comment lines in green, numbers and times, and the line numbers dimmed so the statements stand out. `%X%` abbreviations from `DEFINE` are marked inside point names. A program's own local points in quotes (`"$TMR"`), `DEFINE`'s text and `OIP` keystroke strings stay plain, as in Desigo.
- What can't be right shows in red: a line number out of order (or used twice), a quote left open, a `GOTO` or `GOSUB` without a line number, an operator PPCL doesn't have (`.NOT.`), a priority it doesn't have, and a name over 6 characters without quotes. Lines turned off (`# 00100 …`) and statements the compiler couldn't read (`UNKNOWN (…)`) are dimmed.
- The `.txt` files Desigo exports programs to are recognized by their numbered lines (five digits, or short numbers like `10`), and so is a program pasted into a new tab. Files named `.ppcl`, and `.pcl` files that read like PPCL, open as PPCL too; *Format → Language → PPCL* picks it by hand.
- *Toggle comment* (Ctrl+/) comments lines out the PPCL way: the `C` goes after each line's number (`00010     C SET(…)`), and comes off again the same way.
- Markdown code blocks marked `ppcl` are colored as PPCL.

### Dark mode
- The prompts (save changes, reload, the update, About, error messages) are drawn in Slate's colors now instead of staying light when Slate is dark. They're still Windows' own buttons and text: Tab, Enter, Esc, the underlined letters, screen readers and Ctrl+C (copies the message) work as before. The Open and Save As windows are Windows' own and follow Windows' light or dark setting.
