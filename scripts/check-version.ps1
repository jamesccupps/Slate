# Checks that Slate.exe's version can be read the way the copies out there read it before they install an update
# (src/ui/update.rs, file_version). Slate 0.16.1 and 0.17.0 take the FIRST place in the download where VS_FIXEDFILEINFO's
# head (its signature and structure version, 8 bytes) is, and the version after it. Their own code held those 8 bytes
# as a constant, which came before the resources, so they refused every update. So: the first place they are has to be
# in the resource section, and give this version (Cargo.toml's unless -Version says otherwise). Run after every
# release build (CI's, and the signed exe again).
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Version
)
$ErrorActionPreference = 'Stop'
if (-not $Version) {
    $toml = Get-Content (Join-Path $PSScriptRoot '..\Cargo.toml') -Raw
    if ($toml -notmatch '(?m)^version = "(\d+\.\d+\.\d+)"') { throw "No version in Cargo.toml" }
    $Version = $Matches[1]
}
$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $Exe))
# (bytes as characters one for one, for a quick ordinal search)
$text = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
$head = [System.Text.Encoding]::GetEncoding(28591).GetString([byte[]](0xBD, 0x04, 0xEF, 0xFE, 0x00, 0x00, 0x01, 0x00))
$places = @()
$i = $text.IndexOf($head, [StringComparison]::Ordinal)
while ($i -ge 0) {
    $places += $i
    $i = $text.IndexOf($head, $i + 1, [StringComparison]::Ordinal)
}
if ($places.Count -eq 0) { throw "$Exe has no version resource" }
# the resource section's place in the file
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
$count = [BitConverter]::ToUInt16($bytes, $pe + 6)
$optional = [BitConverter]::ToUInt16($bytes, $pe + 20)
$rsrc = $null
for ($k = 0; $k -lt $count; $k++) {
    $s = $pe + 24 + $optional + $k * 40
    if ([System.Text.Encoding]::ASCII.GetString($bytes, $s, 5) -eq '.rsrc') {
        $rsrc = @([BitConverter]::ToUInt32($bytes, $s + 20), [BitConverter]::ToUInt32($bytes, $s + 16))
    }
}
if (-not $rsrc) { throw "$Exe has no resource section" }
$first = $places[0]
$ms = [BitConverter]::ToUInt32($bytes, $first + 8)
$ls = [BitConverter]::ToUInt32($bytes, $first + 12)
$read = '{0}.{1}.{2}' -f ($ms -shr 16), ($ms -band 0xFFFF), ($ls -shr 16)
$inside = $first -ge $rsrc[0] -and $first -lt $rsrc[0] + $rsrc[1]
if (-not $inside -or $read -ne $Version) {
    throw ("{0}: the copies out there (0.16.1, 0.17.0) would read version {1} at 0x{2:X} ({3}), not {4}: they would refuse " +
        "it as an update. The head of VS_FIXEDFILEINFO is in it {5} times; it must come first in the resources.") -f
        $Exe, $read, $first, $(if ($inside) { 'in the resources' } else { 'before the resources' }), $Version, $places.Count
}
"{0}: version {1} as the copies out there read it (the head {2} time(s), first in the resources)" -f $Exe, $read, $places.Count
