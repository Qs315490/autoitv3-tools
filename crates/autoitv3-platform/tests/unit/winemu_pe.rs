//! Unit tests for `winfmt::pe`'s PE resource reader.
//!
//! Kept out of `pe.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;

#[test]
fn a_non_pe_file_is_rejected() {
    let err = parse(b"not a pe file at all").unwrap_err();
    assert!(err.contains("MZ"), "{err}");
}

#[test]
fn selector_matching_is_case_insensitive_for_names() {
    let sel = Selector::name("payload");
    assert!(sel.matches(None, Some("PAYLOAD")));
    assert!(!sel.matches(Some(10), None));
    assert!(Selector::id(10).matches(Some(10), None));
}

/// A minimal PE32+ image whose only resource is `RT_RCDATA/1`.
///
/// Enough of a file for `parse` to walk the resource tree, so the resource
/// lookup and the discovery order can be tested without shipping a real
/// `.exe` as a fixture.
fn tiny_pe(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 0x400];
    // DOS header.
    f[0..2].copy_from_slice(b"MZ");
    f[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    // PE signature + COFF header.
    f[0x40..0x44].copy_from_slice(b"PE\0\0");
    f[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes()); // machine
    f[0x46..0x48].copy_from_slice(&1u16.to_le_bytes()); // sections
    f[0x54..0x56].copy_from_slice(&0xf0u16.to_le_bytes()); // optional size
    // Optional header: PE32+, one data directory (the resource table).
    let optional = 0x58;
    f[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    f[optional + 108..optional + 112].copy_from_slice(&2u32.to_le_bytes()); // #dirs
    let dirs = optional + 112;
    f[dirs + 16..dirs + 20].copy_from_slice(&0x1000u32.to_le_bytes()); // rsrc RVA
    f[dirs + 20..dirs + 24].copy_from_slice(&0x200u32.to_le_bytes()); // rsrc size
    // Section table: `.rsrc` at RVA 0x1000 / file 0x200.
    let sec = optional + 0xf0;
    f[sec..sec + 5].copy_from_slice(b".rsrc");
    f[sec + 8..sec + 12].copy_from_slice(&0x200u32.to_le_bytes()); // vsize
    f[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // vaddr
    f[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes()); // raw size
    f[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes()); // raw ptr
    // Resource tree: type 10 -> name 1 -> language 0 -> data entry.
    let base = 0x200usize;
    let dir = |f: &mut Vec<u8>, at: usize, entries: u16| {
        f[at + 12..at + 14].copy_from_slice(&0u16.to_le_bytes()); // named
        f[at + 14..at + 16].copy_from_slice(&entries.to_le_bytes()); // ids
    };
    let entry = |f: &mut Vec<u8>, at: usize, name: u32, child: u32| {
        f[at..at + 4].copy_from_slice(&name.to_le_bytes());
        f[at + 4..at + 8].copy_from_slice(&child.to_le_bytes());
    };
    dir(&mut f, base, 1);
    entry(&mut f, base + 16, 10, 0x8000_0000 | 0x18);
    dir(&mut f, base + 0x18, 1);
    entry(&mut f, base + 0x28, 1, 0x8000_0000 | 0x30);
    dir(&mut f, base + 0x30, 1);
    entry(&mut f, base + 0x40, 0, 0x48);
    // Data entry: the payload lives at RVA 0x1100 (file 0x300).
    let data = base + 0x48;
    f[data..data + 4].copy_from_slice(&0x1100u32.to_le_bytes());
    f[data + 4..data + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    f[0x300..0x300 + payload.len()].copy_from_slice(payload);
    f
}

#[test]
fn a_synthetic_image_yields_its_resource() {
    let resources = parse(&tiny_pe(b"hello")).expect("parses");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].data, b"hello");
    assert_eq!(resources[0].type_sel, Selector::id(10));
    assert_eq!(resources[0].name_sel, Selector::id(1));
}

#[test]
fn staged_resource_files_are_found_by_name() {
    let dir = std::env::temp_dir().join(format!("au3-res-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("__Res64")).unwrap();
    std::fs::create_dir_all(dir.join("__ResImage")).unwrap();
    std::fs::write(dir.join("__PLAIN"), b"plain").unwrap();
    std::fs::write(dir.join("__Res64").join("PAY64"), b"payload64").unwrap();
    std::fs::write(dir.join("__ResImage").join("_IMG"), b"image").unwrap();
    let dirs = vec![dir.clone()];

    // `AutoIt3Wrapper_Res_File_Add` staging names, in the three shapes.
    let get = |n: &str| PeImage::find_resource_file(&dirs, &Selector::name(n));
    assert_eq!(get("PLAIN").as_deref(), Some(&b"plain"[..]));
    assert_eq!(get("PAY64").as_deref(), Some(&b"payload64"[..]));
    assert_eq!(get("IMG").as_deref(), Some(&b"image"[..]));
    // `FindResourceW` matches case-insensitively.
    assert_eq!(get("plain").as_deref(), Some(&b"plain"[..]));
    assert!(get("MISSING").is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_prefers_the_image_named_after_the_script() {
    let dir = std::env::temp_dir().join(format!("au3-pe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Sorted first, and *not* the script's name: the name wins.
    std::fs::write(dir.join("AAA.exe"), tiny_pe(b"other")).unwrap();
    std::fs::write(dir.join("tool.exe"), tiny_pe(b"payload")).unwrap();
    std::fs::write(dir.join("notes.txt"), b"not an image").unwrap();
    std::fs::write(dir.join("broken.exe"), b"MZ but truncated").unwrap();

    let found = PeImage::find_resource_module(&dir, Some("tool")).unwrap();
    assert_eq!(found.file_name().unwrap(), "tool.exe");
    // Without a stem the first usable image in name order is chosen.
    let found = PeImage::find_resource_module(&dir, None).unwrap();
    assert_eq!(found.file_name().unwrap(), "AAA.exe");
    // A directory with no `FindResourceW` error looks like this:
    assert!(PeImage::find_resource_module(std::env::temp_dir().join("nope-not-here"), None).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
