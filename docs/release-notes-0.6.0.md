## Slate 0.6.0

Seven more languages: Dart, Scala, Objective-C and Objective-C++, G-code, and the Inno Setup and NSIS installer scripts.

**Download:** `Slate.exe` below, for 64-bit Windows 10 or 11. If you have 0.2.0 or newer, it offers this update in its status bar by itself (*Help → Check for updates* asks right away).

### New languages
- **Dart** (`.dart`) and **Scala** (`.scala`, `.sc`, `.sbt`): keywords and types, strings with `$name` and `${…}` colored inside them, nested comments.
- **Objective-C** (`.m` and `.h` files with `#import`, `@interface` and the like, so a C header or a MATLAB file stays what it is) and **Objective-C++** (`.mm`): the `@` words, `@"…"` strings and `@42`-style literals, `YES`, `nil`, `self`.
- **G-code** (`.gcode`, `.gco`, `.ngc`, `.nc`, `.cnc`, and a slicer's output whatever its name): `G` and `M` codes, parameters, line numbers, comments, `M117` messages, Klipper commands.
- **Inno Setup** (`.iss`, `.isl`): sections, `Name: value;` parameters, `{app}`-style constants, the preprocessor, and the `[Code]` section as Pascal Script. *Toggle comment* uses `;` outside `[Code]` and `//` inside it.
- **NSIS** (`.nsi`, `.nsh`): commands, `!` directives, `$VARIABLES`, `${defines}` and `$(strings)` (inside quotes too), labels, and lines continued with `\`.
- Each one is in *Format → Language*, works with *Toggle comment*, colors Markdown code blocks marked with its name, and is offered in "Open with" once *Help → Open files with Slate* has set that up (so is `.ppcl`).
- None of them colors slower than C does, and the languages that were already there are as fast as before.
