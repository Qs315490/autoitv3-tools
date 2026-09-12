//! Unit tests for `winemu::compress`'s LZNT1 decompressor.
//!
//! Kept out of `compress.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;

/// Build a compressed chunk from `(literal, phrase)` items.
fn chunk(items: &[Item]) -> Vec<u8> {
    let mut body = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let mut flags = 0u8;
        let mut group = Vec::new();
        for bit in 0..8 {
            let Some(item) = items.get(i + bit) else { break };
            match item {
                Item::Literal(b) => group.push(*b),
                Item::Phrase { offset, length } => {
                    flags |= 1 << bit;
                    let token = (((length - 3) as u16) << 12) | ((offset - 1) as u16);
                    group.extend_from_slice(&token.to_le_bytes());
                }
            }
        }
        body.push(flags);
        body.extend_from_slice(&group);
        i += 8;
    }
    let mut out = Vec::new();
    let header = 0x3000u16 | 0x8000 | (body.len() as u16 - 1);
    out.extend_from_slice(&header.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

enum Item {
    Literal(u8),
    Phrase { offset: usize, length: usize },
}

#[test]
fn literals_round_trip() {
    let data = b"hello, lznt1";
    let items: Vec<Item> = data.iter().map(|b| Item::Literal(*b)).collect();
    assert_eq!(decompress(&chunk(&items)).unwrap(), data.to_vec());
}

#[test]
fn a_phrase_copies_from_earlier_output() {
    // "ab" then a phrase {offset: 2, length: 6} repeats "ab" three times.
    let items = vec![
        Item::Literal(b'a'),
        Item::Literal(b'b'),
        Item::Phrase { offset: 2, length: 6 },
    ];
    assert_eq!(decompress(&chunk(&items)).unwrap(), b"abababab".to_vec());
}

#[test]
fn a_bad_signature_is_rejected() {
    assert!(decompress(&[0x00, 0x00, 0xff]).is_some(), "zero header ends the stream");
    assert!(decompress(&[0x11, 0x22, 0x33]).is_none());
}
