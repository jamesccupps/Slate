// Embeds the Windows resources (manifest, icon, version info) into Slate.exe.
//
// The GNU toolchain has no resource compiler (rc.exe / windres), so this writes the COFF object with a .rsrc
// section itself and hands it to the linker. Format: PE/COFF spec, "The .rsrc Section".

use std::env;
use std::fs;
use std::path::PathBuf;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_VERSION: u32 = 16;
const RT_MANIFEST: u32 = 24;
const LANG_EN_US: u32 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/slate.manifest");
    println!("cargo:rerun-if-changed=res/slate.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // The commit it's built from (crash reports, About): empty outside a git checkout.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    println!("cargo:rustc-env=SLATE_COMMIT={}", git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_default());
    // Built again when HEAD moves (also in a worktree, and when the branch is in packed-refs).
    let branch = git(&["rev-parse", "--symbolic-full-name", "HEAD"]).filter(|r| r.starts_with("refs/"));
    let watched = [Some("HEAD".to_string()), branch, Some("packed-refs".to_string())];
    for p in watched.iter().flatten().filter_map(|p| git(&["rev-parse", "--git-path", p])) {
        if std::path::Path::new(&p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut res: Vec<(u32, u32, Vec<u8>)> = Vec::new();

    let manifest = fs::read("res/slate.manifest").expect("res/slate.manifest");
    res.push((RT_MANIFEST, 1, manifest));

    if let Ok(ico) = fs::read("res/slate.ico") {
        let (images, group) = split_ico(&ico);
        for (i, img) in images.into_iter().enumerate() {
            res.push((RT_ICON, i as u32 + 1, img));
        }
        res.push((RT_GROUP_ICON, 1, group));
    }
    res.push((RT_VERSION, 1, version_info(&version)));

    let obj = coff_rsrc(&res);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("slate-res.o");
    fs::write(&out, obj).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// Splits an .ico file into the RT_ICON images and the RT_GROUP_ICON directory that points at them.
fn split_ico(ico: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let u16_at = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]);
    assert!(u16_at(2) == 1, "res/slate.ico is not an icon file");
    let count = u16_at(4) as usize;
    let mut images = Vec::new();
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32_at(e + 8) as usize;
        let offset = u32_at(e + 12) as usize;
        images.push(ico[offset..offset + size].to_vec());
        group.extend_from_slice(&ico[e..e + 8]); // width, height, colors, reserved, planes, bit count
        group.extend_from_slice(&(size as u32).to_le_bytes());
        group.extend_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    (images, group)
}

fn utf16z(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    for u in s.encode_utf16().chain(std::iter::once(0)) {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

fn pad4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// One VS_VERSIONINFO-style node: header, key, value, children (each DWORD aligned).
fn vnode(key: &str, value: &[u8], value_len: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    v.extend_from_slice(&utf16z(key));
    pad4(&mut v);
    v.extend_from_slice(value);
    for c in children {
        pad4(&mut v);
        v.extend_from_slice(c);
    }
    let len = v.len() as u16;
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v[2..4].copy_from_slice(&value_len.to_le_bytes());
    v[4..6].copy_from_slice(&(text as u16).to_le_bytes());
    v
}

fn version_info(version: &str) -> Vec<u8> {
    let parts: Vec<u16> = version.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (parts[0] as u32, parts[1] as u32, parts[2] as u32);
    let mut fixed = Vec::new();
    for d in [
        0xFEEF04BDu32,
        0x0001_0000,
        (a << 16) | b,
        c << 16,
        (a << 16) | b,
        c << 16,
        0x3F,
        0,
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend_from_slice(&d.to_le_bytes());
    }
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "Slate"),
        ("FileDescription", "Slate"),
        ("FileVersion", version),
        ("InternalName", "Slate"),
        ("LegalCopyright", "Copyright (c) 2026 jamesccupps. MIT license."),
        ("OriginalFilename", "Slate.exe"),
        ("ProductName", "Slate"),
        ("ProductVersion", version),
    ]
    .iter()
    .map(|(k, val)| {
        let w = utf16z(val);
        vnode(k, &w, (w.len() / 2) as u16, true, &[])
    })
    .collect();
    let table = vnode("040904B0", &[], 0, true, &strings);
    let sfi = vnode("StringFileInfo", &[], 0, true, &[table]);
    let translation = 0x04B0_0409u32.to_le_bytes();
    let var = vnode("Translation", &translation, 4, false, &[]);
    let vfi = vnode("VarFileInfo", &[], 0, true, &[var]);
    vnode("VS_VERSION_INFO", &fixed, fixed.len() as u16, false, &[sfi, vfi])
}

/// Builds a COFF object (x86-64) whose .rsrc section holds the given (type, id, data) resources, all en-US.
fn coff_rsrc(res: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut types: Vec<u32> = res.iter().map(|r| r.0).collect();
    types.sort();
    types.dedup();

    // Layout: root dir, type dirs, name dirs (one language each), data entries, then the data.
    let dir_size = |n: usize| 16 + 8 * n;
    let root_size = dir_size(types.len());
    let mut offset = root_size;
    let mut type_dir_off = Vec::new();
    for t in &types {
        type_dir_off.push(offset);
        offset += dir_size(res.iter().filter(|r| r.0 == *t).count());
    }
    // Resources ordered by (type, id), which is the order the directories list them.
    let mut order: Vec<usize> = (0..res.len()).collect();
    order.sort_by_key(|&i| (res[i].0, res[i].1));
    let mut name_dir_off = vec![0; res.len()];
    for &i in &order {
        name_dir_off[i] = offset;
        offset += dir_size(1);
    }
    let mut entry_off = vec![0; res.len()];
    for &i in &order {
        entry_off[i] = offset;
        offset += 16;
    }
    let mut data_off = vec![0; res.len()];
    for &i in &order {
        offset = (offset + 7) & !7;
        data_off[i] = offset;
        offset += res[i].2.len();
    }
    let total = (offset + 7) & !7;

    let mut sec = vec![0u8; total];
    let put16 = |s: &mut Vec<u8>, o: usize, v: u16| s[o..o + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |s: &mut Vec<u8>, o: usize, v: u32| s[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let dir = |s: &mut Vec<u8>, o: usize, ids: &[(u32, u32)]| {
        // Characteristics, TimeDateStamp, Major/MinorVersion stay zero.
        put16(s, o + 14, ids.len() as u16);
        for (k, (id, target)) in ids.iter().enumerate() {
            put32(s, o + 16 + 8 * k, *id);
            put32(s, o + 16 + 8 * k + 4, *target);
        }
    };
    let root: Vec<(u32, u32)> =
        types.iter().zip(&type_dir_off).map(|(t, o)| (*t, 0x8000_0000 | *o as u32)).collect();
    dir(&mut sec, 0, &root);
    for (t, off) in types.iter().zip(&type_dir_off) {
        let ids: Vec<(u32, u32)> =
            order.iter().filter(|&&i| res[i].0 == *t).map(|&i| (res[i].1, 0x8000_0000 | name_dir_off[i] as u32)).collect();
        dir(&mut sec, *off, &ids);
    }
    let mut relocs = Vec::new();
    for &i in &order {
        dir(&mut sec, name_dir_off[i], &[(LANG_EN_US, entry_off[i] as u32)]);
        put32(&mut sec, entry_off[i], data_off[i] as u32); // made image-relative by the relocation below
        put32(&mut sec, entry_off[i] + 4, res[i].2.len() as u32);
        relocs.push(entry_off[i] as u32);
        sec[data_off[i]..data_off[i] + res[i].2.len()].copy_from_slice(&res[i].2);
    }

    let mut obj = Vec::new();
    let raw_ptr = 20 + 40;
    let reloc_ptr = raw_ptr + sec.len();
    let sym_ptr = reloc_ptr + relocs.len() * 10;
    // IMAGE_FILE_HEADER
    obj.extend_from_slice(&0x8664u16.to_le_bytes());
    obj.extend_from_slice(&1u16.to_le_bytes());
    obj.extend_from_slice(&0u32.to_le_bytes());
    obj.extend_from_slice(&(sym_ptr as u32).to_le_bytes());
    obj.extend_from_slice(&1u32.to_le_bytes());
    obj.extend_from_slice(&0u16.to_le_bytes());
    obj.extend_from_slice(&0u16.to_le_bytes());
    // IMAGE_SECTION_HEADER
    obj.extend_from_slice(b".rsrc\0\0\0");
    obj.extend_from_slice(&0u32.to_le_bytes());
    obj.extend_from_slice(&0u32.to_le_bytes());
    obj.extend_from_slice(&(sec.len() as u32).to_le_bytes());
    obj.extend_from_slice(&(raw_ptr as u32).to_le_bytes());
    obj.extend_from_slice(&(reloc_ptr as u32).to_le_bytes());
    obj.extend_from_slice(&0u32.to_le_bytes());
    obj.extend_from_slice(&(relocs.len() as u16).to_le_bytes());
    obj.extend_from_slice(&0u16.to_le_bytes());
    obj.extend_from_slice(&0x4000_0040u32.to_le_bytes()); // initialized data, readable
    obj.extend_from_slice(&sec);
    for r in relocs {
        obj.extend_from_slice(&r.to_le_bytes());
        obj.extend_from_slice(&0u32.to_le_bytes()); // symbol 0 = the section
        obj.extend_from_slice(&3u16.to_le_bytes()); // IMAGE_REL_AMD64_ADDR32NB
    }
    // Symbol table: the section symbol, then an empty string table.
    obj.extend_from_slice(b".rsrc\0\0\0");
    obj.extend_from_slice(&0u32.to_le_bytes());
    obj.extend_from_slice(&1i16.to_le_bytes());
    obj.extend_from_slice(&0u16.to_le_bytes());
    obj.push(3); // IMAGE_SYM_CLASS_STATIC
    obj.push(0);
    obj.extend_from_slice(&4u32.to_le_bytes());
    obj
}
