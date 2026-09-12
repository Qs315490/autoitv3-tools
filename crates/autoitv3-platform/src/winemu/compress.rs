//! LZNT1 — the compression `RtlDecompressBuffer` speaks.
//!
//! The sample hands its embedded resources to
//! `RtlGetCompressionWorkSpaceSize` / `RtlDecompressBuffer` with
//! `COMPRESSION_FORMAT_LZNT1` (`2`) before decrypting them, so the format has
//! to be understood off Windows too.
//!
//! The layout, per the `[MS-XCA]` description of LZNT1:
//!
//! * the stream is a sequence of chunks, each starting with a 2-byte header;
//! * the header's bits 12–14 are the signature `0b011`, bit 15 says whether the
//!   chunk is compressed and bits 0–11 are `length - 1`;
//! * a compressed chunk is a series of flag bytes, each covering up to eight
//!   items: a `0` bit is a literal byte, a `1` bit is a 2-byte phrase whose low
//!   12 bits are `offset - 1` and whose high nibble is `length - 3` (a nibble of
//!   `0xF` continues into extra bytes);
//! * a phrase copies from the current chunk's output, so it may overlap.

/// `COMPRESSION_FORMAT_LZNT1`.
pub const COMPRESSION_FORMAT_LZNT1: u32 = 2;

/// Decompress an LZNT1 stream.
///
/// Returns `None` for input that does not look like LZNT1, so the caller can
/// fail the call instead of handing back half-decoded bytes.
pub fn decompress(input: &[u8]) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;

    while pos + 2 <= input.len() {
        let header = u16::from_le_bytes([input[pos], input[pos + 1]]);
        pos += 2;
        if header == 0 {
            break;
        }
        // Signature bits must read 0b011; anything else is not this format.
        if header & 0x7000 != 0x3000 {
            return None;
        }
        let length = (header & 0x0FFF) as usize + 1;
        let compressed = header & 0x8000 != 0;
        let end = pos.checked_add(length)?;
        if end > input.len() {
            return None;
        }
        let chunk = &input[pos..end];
        pos = end;

        if !compressed {
            out.extend_from_slice(chunk);
            continue;
        }

        // Phrases may not reach before this chunk's output.
        let chunk_start = out.len();
        let mut i = 0usize;
        while i < chunk.len() {
            let flags = chunk[i];
            i += 1;
            for bit in 0..8 {
                if i >= chunk.len() {
                    break;
                }
                if flags & (1 << bit) == 0 {
                    out.push(chunk[i]);
                    i += 1;
                    continue;
                }
                if i + 1 >= chunk.len() {
                    return None;
                }
                let token = u16::from_le_bytes([chunk[i], chunk[i + 1]]);
                i += 2;
                let offset = (token & 0x0FFF) as usize + 1;
                let mut length = (token >> 12) as usize;
                if length == 0x0F {
                    loop {
                        let ext = *chunk.get(i)?;
                        i += 1;
                        length += ext as usize;
                        if ext != 0xFF {
                            break;
                        }
                    }
                }
                let length = length + 3;
                if offset > out.len() - chunk_start {
                    return None;
                }
                // Byte-by-byte: an overlapping copy is the point of LZNT1.
                for _ in 0..length {
                    let byte = out[out.len() - offset];
                    out.push(byte);
                }
            }
        }
    }

    Some(out)
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/winemu_compress.rs"]
mod tests;
