//! Unit tests for `script::lz`'s decompressor, including its edge cases.
//!
//! Kept out of `lz.rs` so the module reads as implementation;
//! `#[path]` pulls the file back in as a unit-test module, which is what
//! lets it reach private state the module does not expose.

use super::*;

/// Build a bit stream most-significant-bit first, the way the format
/// expects to read it.
struct BitWriter {
    bytes: Vec<u8>,
    position: usize,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { bytes: Vec::new(), position: 0 }
    }

    fn put(&mut self, width: usize, value: u32) {
        for i in (0..width).rev() {
            if self.position % 8 == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> i) & 1) as u8;
            let last = self.bytes.len() - 1;
            self.bytes[last] |= bit << (7 - self.position % 8);
            self.position += 1;
        }
    }

    fn literal(&mut self, byte: u8) {
        self.literal_with(byte, 1);
    }

    /// A literal with an explicit polarity, for building `EA05` streams.
    fn literal_with(&mut self, byte: u8, polarity: u32) {
        self.put(1, polarity);
        self.put(8, byte as u32);
    }

    fn reference(&mut self, offset: usize, length: usize) {
        self.put(1, 0);
        self.put(15, offset as u32);
        // Every test length is small enough for the first step.
        assert!(length >= 3 && length <= 6, "use a bigger step in the test");
        self.put(2, (length - 3) as u32);
    }

    /// A complete blob with the header the decompressor expects.
    fn blob(self, uncompressed_size: usize) -> Vec<u8> {
        let mut out = b"EA06".to_vec();
        out.extend_from_slice(&(uncompressed_size as u32).to_be_bytes());
        out.extend_from_slice(&self.bytes);
        out
    }
}

#[test]
fn literals_come_back_out() {
    let mut w = BitWriter::new();
    for byte in b"Hello" {
        w.literal(*byte);
    }
    let blob = w.blob(5);
    assert_eq!(decompress(&blob, true).unwrap(), b"Hello");
}

#[test]
fn a_reference_copies_from_behind_the_cursor() {
    let mut w = BitWriter::new();
    w.literal(b'a');
    w.literal(b'b');
    w.literal(b'c');
    // Distance 3, length 3: copies `abc` again.
    w.reference(3, 3);
    let blob = w.blob(6);
    assert_eq!(decompress(&blob, true).unwrap(), b"abcabc");
}

#[test]
fn an_overlapping_reference_repeats_the_piece() {
    let mut w = BitWriter::new();
    w.literal(b'x');
    // Distance 1, length 5: the reference reaches past the cursor, so the
    // byte it just wrote is repeated.
    w.reference(1, 5);
    let blob = w.blob(6);
    assert_eq!(decompress(&blob, true).unwrap(), b"xxxxxx");
}

#[test]
fn the_signature_has_to_match_the_version() {
    // The same content written with each version's literal polarity.
    let mut ea06_writer = BitWriter::new();
    ea06_writer.literal(b'a');
    let ea06 = ea06_writer.blob(1);
    assert!(decompress(&ea06, true).is_ok());
    assert!(decompress(&ea06, false).is_err(), "EA06 blob read as EA05");

    let mut ea05_writer = BitWriter::new();
    ea05_writer.literal_with(b'a', 0);
    let mut ea05 = ea05_writer.blob(1);
    ea05[..4].copy_from_slice(b"EA05");
    assert_eq!(decompress(&ea05, false).unwrap(), b"a");
    assert!(decompress(&ea05, true).is_err(), "EA05 blob read as EA06");
}

#[test]
fn a_truncated_stream_is_an_error_not_a_panic() {
    let mut w = BitWriter::new();
    w.literal(b'a');
    let mut blob = w.blob(4); // claims four bytes, supplies one
    blob.truncate(blob.len() - 1);
    assert!(decompress(&blob, true).is_err());
}

#[test]
fn an_absurd_size_header_is_refused() {
    let mut blob = b"EA06".to_vec();
    blob.extend_from_slice(&u32::MAX.to_be_bytes());
    assert!(decompress(&blob, true).is_err());
}
