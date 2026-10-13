# Points Slate.exe's load configuration at the structure src/main.rs puts in it (`_load_config_used`): GNU ld doesn't
# fill in that directory itself, and without it Windows doesn't see DependentLoadFlags = LOAD_LIBRARY_SEARCH_SYSTEM32,
# so it looks for the DLLs Slate.exe imports in Slate.exe's own folder first (a DLL a web page got saved into
# Downloads would be loaded when Slate.exe is started from there). Run on every Windows build before it's tested or
# signed (build.yml does); it fails unless the structure is in the exe exactly once. Recomputes the PE checksum.
param([Parameter(Mandatory)] [string] $Exe)
$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.IO;
public static class SlateLoadConfig {
    static uint U32(byte[] b, int o) { return BitConverter.ToUInt32(b, o); }
    static int U16(byte[] b, int o) { return BitConverter.ToUInt16(b, o); }
    static void Put(byte[] b, int o, uint v) { BitConverter.GetBytes(v).CopyTo(b, o); }
    // The structure: Size = 112, DependentLoadFlags = 0x800 at offset 78, every other field zero.
    static bool IsIt(byte[] b, int at) {
        if (U32(b, at) != 112 || U16(b, at + 78) != 0x800) return false;
        for (int i = 4; i < 112; i++) if ((i < 78 || i > 79) && b[at + i] != 0) return false;
        return true;
    }
    public static string Run(string path) {
        byte[] b = File.ReadAllBytes(path);
        int pe = (int)U32(b, 0x3C);
        if (U32(b, pe) != 0x4550) throw new Exception("not a PE file");
        int opt = pe + 24;
        if (U16(b, opt) != 0x20B) throw new Exception("not a 64-bit PE file");
        if (U32(b, opt + 108) < 11) throw new Exception("no room for a load configuration");
        int at = -1;
        for (int i = 0; i + 112 <= b.Length; i += 8) {
            if (IsIt(b, i)) {
                if (at >= 0) throw new Exception("the load configuration structure is there more than once");
                at = i;
            }
        }
        if (at < 0) throw new Exception("the load configuration structure isn't there (src/main.rs)");
        int nsec = U16(b, pe + 6), sec = opt + U16(b, pe + 20);
        long rva = -1;
        for (int i = 0; i < nsec; i++) {
            int s = sec + 40 * i;
            uint va = U32(b, s + 12), rsz = U32(b, s + 16), raw = U32(b, s + 20);
            if (at >= raw && at + 112 <= raw + rsz) rva = at - raw + va;
        }
        if (rva < 0) throw new Exception("the structure isn't in a section");
        int dir = opt + 112 + 10 * 8;
        uint old = U32(b, dir);
        if (old != 0 && old != rva) throw new Exception("the exe has another load configuration already");
        Put(b, dir, (uint)rva);
        Put(b, dir + 4, 112);
        // The PE checksum, as ImageHlp's CheckSumMappedFile: 16-bit words with the field itself as zero, plus the length.
        Put(b, opt + 64, 0);
        ulong sum = 0;
        for (int i = 0; i + 1 < b.Length; i += 2) { sum += BitConverter.ToUInt16(b, i); sum = (sum & 0xFFFF) + (sum >> 16); }
        if ((b.Length & 1) != 0) { sum += b[b.Length - 1]; sum = (sum & 0xFFFF) + (sum >> 16); }
        Put(b, opt + 64, (uint)(sum + (ulong)b.Length));
        File.WriteAllBytes(path, b);
        return String.Format("load configuration at 0x{0:x} (DependentLoadFlags = LOAD_LIBRARY_SEARCH_SYSTEM32)", rva);
    }
}
"@
[SlateLoadConfig]::Run((Resolve-Path $Exe).Path)
