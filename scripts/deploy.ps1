# Copies Slate.exe into an install folder. A running Slate there is renamed aside first (Windows allows renaming a
# running exe, not overwriting it); Slate deletes such leftovers itself on its next start.
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Dir,
    # Keep settings, the session and big temp files in "data" beside Slate.exe instead of AppData / %TEMP%.
    [switch] $Portable
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$dest = Join-Path $Dir 'Slate.exe'
if (Test-Path $dest) {
    if ((Get-FileHash $dest).Hash -eq (Get-FileHash $Exe).Hash) { "Already up to date: $dest"; return }
    Rename-Item -Path $dest -NewName ("Slate.old-{0}.exe" -f (Get-Date -Format 'yyyyMMddHHmmss'))
}
Copy-Item -Path $Exe -Destination $dest
if ($Portable) {
    $marker = Join-Path $Dir 'Slate.portable'
    if (-not (Test-Path $marker)) {
        Set-Content -Path $marker -Encoding ascii -Value 'Slate keeps its settings, session and temporary files in the "data" folder next to this file.'
    }
}
"Deployed $dest"
