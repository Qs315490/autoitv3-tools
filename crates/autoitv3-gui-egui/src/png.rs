//! A tiny PNG writer.
//!
//! Offscreen screenshots need a portable image file, and a PNG with *stored*
//! (uncompressed) deflate blocks is a few dozen lines — no image crate, no
//! compression dependency, and byte-for-byte deterministic output.

use std::io;

/// Write `rgba` (8-bit RGBA, row-major) to `path` as a PNG.
pub fn write_png(
    path: impl AsRef<std::path::Path>,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> io::Result<()> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected {expected} bytes, got {}", rgba.len()),
        ));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, deflate, no filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);

    // Each scanline is prefixed with filter type 0 (None).
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        let start = y * width as usize * 4;
        raw.extend_from_slice(&rgba[start..start + width as usize * 4]);
    }
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// A zlib stream whose deflate blocks are all "stored".
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.push(1);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    } else {
        let mut i = 0;
        while i < data.len() {
            let n = (data.len() - i).min(0xFFFF);
            let last = i + n == data.len();
            out.push(u8::from(last));
            out.extend_from_slice(&(n as u16).to_le_bytes());
            out.extend_from_slice(&(!(n as u16)).to_le_bytes());
            out.extend_from_slice(&data[i..i + n]);
            i += n;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

struct Crc32 {
    value: u32,
}

impl Crc32 {
    fn new() -> Self {
        Self { value: 0xFFFF_FFFF }
    }
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let mut c = self.value ^ u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
            self.value = c;
        }
    }
    fn finish(&self) -> u32 {
        self.value ^ 0xFFFF_FFFF
    }
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
