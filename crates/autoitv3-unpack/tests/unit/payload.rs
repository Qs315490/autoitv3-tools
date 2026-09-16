//! Unit tests for the resource-payload half of the crate: index selection, its small AutoIt-semantics helpers, and the end-to-end sample decode.
//!
//! Kept out of `lib.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;

#[test]
fn the_tail_marker_says_how_much_to_strip() {
    // The marker's 1st, 3rd, 5th and 7th characters spell the length:
    // `0A1B0C0D` reads as `0100`, so 256 characters come off the end.
    let text = format!("{}{}", "AB".repeat(256), "0A1B0C0D");
    let (taken, rest) = split_tail(&text).unwrap();
    assert_eq!(taken.len(), 256);
    assert_eq!(rest, "AB".repeat(128));
    assert_eq!(taken, &text[256..512]);
}

#[test]
fn a_short_block_is_rejected_rather_than_panicking() {
    assert!(split_tail("abc").is_err());
}

#[test]
fn wrapping_takes_the_front_of_the_dictionary() {
    let dict: Vec<u8> = (b'a'..=b'e').collect();
    assert_eq!(wrap_take(&dict, 3), b"abc");
    assert_eq!(wrap_take(&dict, 7), b"abcdeab");
    assert!(wrap_take(&[], 4).is_empty());
}

#[test]
fn dictionary_lookups_are_one_based_and_clamped() {
    let dict = "abcdef";
    assert_eq!(take_at(dict, 1, 3), "abc");
    assert_eq!(take_at(dict, 4, 2), "de");
    assert_eq!(take_at(dict, 5, 99), "ef");
    assert_eq!(take_at(dict, 99, 2), "");
}

#[test]
fn dec_reads_hexadecimal_and_zeroes_what_is_not() {
    assert_eq!(dec1('A'), 10);
    assert_eq!(dec1('9'), 9);
    assert_eq!(dec1('Z'), 0);
    assert_eq!(from_hex("00E0"), Some(224));
    assert_eq!(from_hex("FF"), Some(255));
    assert_eq!(from_hex("xy"), None);
}

#[test]
fn entries_can_be_picked_by_the_index_a_disassembly_uses() {
    let entries: Vec<String> = (1..=10).map(|n| format!("entry{n}")).collect();
    let picked = select_entries(&entries, "152").unwrap_err();
    assert!(matches!(picked, Error::BadIndex(_)), "152 is past the end");

    let picked = select_entries(&entries, "3").unwrap();
    assert_eq!(picked, vec![(3, "entry3".to_string())]);
    let picked = select_entries(&entries, "2-4,4,1").unwrap();
    assert_eq!(
        picked.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "ranges are inclusive and duplicates collapse"
    );
    assert!(matches!(
        select_entries(&entries, "4-2").unwrap_err(),
        Error::BadIndex(_)
    ));
    assert!(
        matches!(select_entries(&entries, "0").unwrap_err(), Error::BadIndex(_)),
        "0 is the count, not an entry"
    );
    assert!(matches!(
        select_entries(&entries, "abc").unwrap_err(),
        Error::BadIndex(_)
    ));
}

#[test]
fn junk_is_not_mistaken_for_a_package() {
    let candidates = vec![
        ("A".to_string(), vec![0u8; 64]),
        ("B".to_string(), vec![7u8; 96]),
        ("C".to_string(), vec![9u8; 128]),
        ("D".to_string(), vec![3u8; 160]),
    ];
    assert!(matches!(unpack(&candidates), Err(Error::NotAPackage)));
}

/// A real package, when one is available: `AU3_UNPACK_INPUT` may point
/// at a directory of staged resources or at the `.exe` that carried them.
#[test]
fn a_real_package_decodes_and_verifies_itself() {
    let Some(path) = std::env::var("AU3_UNPACK_INPUT").ok().filter(|p| !p.is_empty()) else {
        return;
    };
    let path = std::path::Path::new(&path);
    let candidates = if path.is_dir() {
        candidates_from_dir(path)
    } else {
        candidates_from_image(path)
    }
    .expect("resources are readable");
    let decoded = unpack(&candidates).expect("a packed payload");
    // The last stage checks a SHA-1 over the plaintext, so reaching this
    // point already proves the decoding is authentic; the count is a
    // sanity check on top.
    assert!(decoded.entries.len() > 100, "only {} entries", decoded.entries.len());
    assert!(!decoded.text.is_empty());
}

#[test]
fn candidates_are_written_out_as_one_file_each() {
    // This is the read-back `#AutoIt3Wrapper_Res_File_Add` files are for: the
    // resource names are what that directive gave, so those names (and only
    // those) come back. A name a filesystem cannot take is flattened, and a
    // repeat gets a suffix instead of overwriting the first file.
    let dir = std::env::temp_dir().join(format!("au3-unpack-write-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let resources = vec![
        ("RCDATA".to_string(), "File_Add.dat".to_string(), vec![1u8, 2, 3]),
        ("RCDATA".to_string(), "file_add.DAT".to_string(), vec![4]),
        ("ICON".to_string(), "1".to_string(), vec![9]),
        ("CUSTOM TYPE".to_string(), String::new(), vec![7]),
    ];
    let written = write_resources(&dir, &resources).expect("writes");
    let rel: Vec<String> = written
        .iter()
        .map(|p| {
            p.strip_prefix(&dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    // One directory per resource type, the resource's own name inside it; a
    // repeat inside a type gets a suffix rather than overwriting the first.
    assert_eq!(
        rel,
        vec![
            "RCDATA/File_Add.dat",
            "RCDATA/file_add.DAT.2",
            "ICON/1",
            "CUSTOM TYPE/resource_4"
        ]
    );
    assert_eq!(std::fs::read(&written[0]).unwrap(), vec![1, 2, 3]);
    assert_eq!(std::fs::read(&written[3]).unwrap(), vec![7]);
    let _ = std::fs::remove_dir_all(&dir);
}
