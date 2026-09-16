//! The LZ77-style packer AutoIt compresses embedded files with.
//!
//! A compressed blob starts with a four-byte signature — `EA05` or `EA06` —
//! and the *uncompressed* size as a big-endian `u32`, then a bit stream read
//! most-significant-bit first. Each item starts with one bit:
//!
//! * the "literal" bit (a `1` for `EA06`, a `0` for `EA05`) means "eight more
//!   bits are a byte to copy out";
//! * the other value means a back-reference: a 15-bit distance back into the
//!   output, then a variable-length match count.
//!
//! The match count is a prefix code over five widths, each saturating at an
//! all-ones value: `2` bits up to `3`, `3` bits up to `7`, `5` bits up to `31`,
//! then `8`-bit steps. A reference may overlap the bytes it is copying, which
//! is how runs are encoded, so the copy is a byte-at-a-time repeat rather than
//! a block move.
//!
//! This is a port of the MIT-licensed AutoIt-Ripper's `decompress.py`.

use autoitv3_i18n::{msg, tr};

/// Refuse a claimed output larger than this. The figure follows the reference
/// implementation; a corrupt header should not be able to ask for gigabytes.
const MAX_UNCOMPRESSED: usize = 10_000_000;

/// Why a compressed blob could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LzError(pub String);

impl std::fmt::Display for LzError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Decompress an `EA05`/`EA06` blob.
///
/// `ea06` selects the literal bit's polarity; the stream's own signature is
/// checked against it, so handing an `EA05` blob to an `EA06` caller fails
/// instead of producing plausible-looking noise.
pub fn decompress(data: &[u8], ea06: bool) -> Result<Vec<u8>, LzError> {
    let expected: &[u8; 4] = if ea06 { b"EA06" } else { b"EA05" };
    let Some(signature) = data.get(..4) else {
        return Err(LzError(tr("the compressed blob is shorter than its signature").into()));
    };
    if signature != expected {
        return Err(LzError(msg!(
            "the compressed blob is not {expected} (starts with {actual} )",
            expected = String::from_utf8_lossy(expected),
            actual = describe(signature)
        )));
    }
    let Some(size_bytes) = data.get(4..8) else {
        return Err(LzError(tr("the compressed blob has no size header").into()));
    };
    let size = u32::from_be_bytes([size_bytes[0], size_bytes[1], size_bytes[2], size_bytes[3]]) as usize;
    if size > MAX_UNCOMPRESSED {
        return Err(LzError(msg!(
            "the compressed blob claims {size} bytes, past the {cap} byte cap",
            size = size,
            cap = MAX_UNCOMPRESSED
        )));
    }

    let mut bits = BitReader::new(&data[8..]);
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let literal = if ea06 { 1 } else { 0 };

    while out.len() < size {
        if bits.take(1)? == literal {
            out.push(bits.take(8)? as u8);
            continue;
        }
        let offset = bits.take(15)? as usize;
        let length = match_length(&mut bits)?;

        // Python's negative slice: a distance past the start clamps to the
        // whole output rather than failing, and `[:length]` clamps the other
        // end. Both are part of the format's observable behaviour.
        let start = out.len().saturating_sub(offset);
        let available = &out[start..];
        let take = available.len().min(length);
        let mut piece = available[..take].to_vec();

        let fillup = length as isize - offset as isize;
        if fillup > 0 {
            let fillup = fillup as usize;
            if piece.is_empty() {
                return Err(LzError(tr("a back-reference copies from before the output").into()));
            }
            if fillup == 1 {
                piece.push(piece[0]);
            } else {
                // Repeat the piece to reach `fillup` bytes, the way the
                // reference's `repeat_cut` does.
                let base = piece.clone();
                for i in 0..fillup {
                    piece.push(base[i % base.len()]);
                }
            }
        }
        out.extend_from_slice(&piece);
    }
    Ok(out)
}

/// The five-step prefix code that carries a match length.
fn match_length(bits: &mut BitReader<'_>) -> Result<usize, LzError> {
    // (base length, bits to read, the value that means "carry on")
    const STEPS: [(usize, usize, u32); 5] = [
        (3, 2, 0b11),
        (6, 3, 0b111),
        (13, 5, 0b11111),
        (44, 8, 255),
        (299, 8, 255),
    ];
    for (base, width, carry) in STEPS {
        let extra = bits.take(width)?;
        if extra != carry {
            return Ok(base + extra as usize);
        }
    }
    // Every step saturated, so the length continues in 255-byte steps.
    let mut base = 299usize;
    loop {
        base += 255;
        let extra = bits.take(8)?;
        if extra != 255 {
            return Ok(base + extra as usize);
        }
    }
}

/// A most-significant-bit-first reader over a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    /// How many bits have been consumed.
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, position: 0 }
    }

    /// Read `width` bits (at most 32) as a big-endian number.
    fn take(&mut self, width: usize) -> Result<u32, LzError> {
        let mut value = 0u32;
        for _ in 0..width {
            let byte = *self
                .data
                .get(self.position / 8)
                .ok_or_else(|| LzError(tr("the compressed stream ends mid-item").into()))?;
            let bit = (byte >> (7 - self.position % 8)) & 1;
            value = (value << 1) | bit as u32;
            self.position += 1;
        }
        Ok(value)
    }
}

/// A short hex-ish rendering of four bytes, for error messages.
fn describe(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/script_lz.rs"]
mod tests;
