# Third-party notices

`Slate.exe` contains code from the projects below. Each is used under the MIT license (most of them offer MIT or
Apache-2.0; aho-corasick and memchr offer MIT or the Unlicense). The MIT license text follows the list.

| Component | Version | Copyright | Source |
|---|---|---|---|
| Rust standard library | the toolchain's | The Rust Project Developers | https://github.com/rust-lang/rust |
| aho-corasick | 1.1.5 | 2015 Andrew Gallant | https://github.com/BurntSushi/aho-corasick |
| bytecount | 0.6.9 | 2017 The bytecount Developers | https://github.com/llogiq/bytecount |
| itoa | 1.0.18 | The itoa authors (David Tolnay) | https://github.com/dtolnay/itoa |
| memchr | 2.8.3 | 2015 Andrew Gallant | https://github.com/BurntSushi/memchr |
| regex, regex-automata, regex-syntax | 1.13.1, 0.4.18, 0.8.11 | 2014 The Rust Project Developers | https://github.com/rust-lang/regex |
| serde, serde_core | 1.0.229 | The serde authors (Erick Tryzelaar, David Tolnay) | https://github.com/serde-rs/serde |
| serde_json | 1.0.151 | The serde_json authors (Erick Tryzelaar, David Tolnay) | https://github.com/serde-rs/json |
| windows, windows-core, windows-result, windows-strings, windows-targets, windows_x86_64_gnu | 0.58.0, 0.58.0, 0.2.0, 0.1.0, 0.52.6, 0.52.6 | Microsoft Corporation | https://github.com/microsoft/windows-rs |
| zmij | 1.0.23 | The zmij authors (David Tolnay) | https://github.com/dtolnay/zmij |

It also contains startup and runtime support code that comes with Rust's GNU toolchain for Windows: from the
MinGW-w64 project (https://www.mingw-w64.org/, whose runtime is in the public domain or under the Zope Public License
2.1) and GCC's libgcc (under the GCC Runtime Library Exception).

## MIT License

Copyright (c) the authors and copyright holders listed above

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, including without limitation the
rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
