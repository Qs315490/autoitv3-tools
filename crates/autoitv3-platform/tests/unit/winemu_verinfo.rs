//! Unit tests for `winfmt::verinfo`'s RT_VERSION reader.
//!
//! Kept out of `verinfo.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;

fn utf16(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    out.extend_from_slice(&[0, 0]);
    out
}

fn block(key: &str, value: &[u8], value_is_string: bool, children: &[Vec<u8>]) -> Vec<u8> {
    // Fields are aligned relative to the start of the resource, and the
    // block header is 6 bytes, so track the absolute offset.
    let mut body: Vec<u8> = Vec::new();
    let mut abs = 6usize;
    let key_bytes = utf16(key);
    body.extend_from_slice(&key_bytes);
    abs += key_bytes.len();
    while abs % 4 != 0 {
        body.push(0);
        abs += 1;
    }
    let value_len = if value_is_string {
        value.len() / 2
    } else {
        value.len()
    };
    body.extend_from_slice(value);
    abs += value.len();
    while abs % 4 != 0 {
        body.push(0);
        abs += 1;
    }
    for child in children {
        body.extend_from_slice(child);
        abs += child.len();
    }
    let total = abs;
    let mut out = Vec::new();
    out.extend_from_slice(&(total as u16).to_le_bytes());
    out.extend_from_slice(&(value_len as u16).to_le_bytes());
    out.extend_from_slice(&(u16::from(value_is_string)).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

#[test]
fn parses_fixed_version_and_string_table() {
    let mut fixed = vec![0u8; 52];
    fixed[8..12].copy_from_slice(&((1u32 << 16) | 2).to_le_bytes());
    fixed[12..16].copy_from_slice(&((3u32 << 16) | 4).to_le_bytes());
    let file_version = utf16("1.2.3.4");
    let string = block("FileVersion", &file_version, true, &[]);
    let lang = block("040904b0", &[], false, &[string]);
    let sfi = block("StringFileInfo", &[], false, &[lang]);
    let root = block("VS_VERSION_INFO", &fixed, false, &[sfi]);

    let info = parse(&root).expect("parses");
    assert_eq!(info.dotted().as_deref(), Some("1.2.3.4"));
    assert_eq!(info.string("fileversion"), Some("1.2.3.4"));
    assert_eq!(info.string("CompanyName"), None);
}
