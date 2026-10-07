@echo off
rem Builds Slate.exe (release) and copies it to dist\.
rem Uses the Rust GNU toolchain from %USERPROFILE%\.cargo (see CLAUDE.md "Build").
rem If a file named .install-dir exists next to this script, Slate is also deployed to the folder named on its
rem first line, in portable mode (settings, session and temp files kept in a "data" folder beside Slate.exe).
setlocal
set "HERE=%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;%PATH%"
cargo build --release --manifest-path "%HERE%Cargo.toml"
if errorlevel 1 exit /b 1
if not exist "%HERE%dist" mkdir "%HERE%dist"
copy /y "%HERE%target\release\Slate.exe" "%HERE%dist\Slate.exe" >nul
echo Built dist\Slate.exe
rem (Not inside an "if (...)" block: there %DEST% would be expanded before set /p reads it.)
if not exist "%HERE%.install-dir" exit /b 0
set /p DEST=<"%HERE%.install-dir"
powershell -NoProfile -ExecutionPolicy Bypass -File "%HERE%scripts\deploy.ps1" -Exe "%HERE%dist\Slate.exe" -Dir "%DEST%" -Portable
