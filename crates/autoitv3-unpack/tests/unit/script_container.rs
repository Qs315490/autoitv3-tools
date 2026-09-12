//! Unit tests for `script`'s record reader: locating a chunk, building one, and the checks that reject a bad one.
//!
//! Kept out of `mod.rs` so the module reads as implementation;
//! `#[path]` pulls the file back in as a unit-test module, which is what
//! lets it reach private state the module does not expose.

use super::*;

/// A minimal container writer: the inverse of [`parse_records`], used to
/// check the reader against data built the way the compiler builds it.
struct Bundle {
    version: ScriptVersion,
    body: Vec<u8>,
}

impl Bundle {
    fn new(version: ScriptVersion) -> Self {
        Bundle { version, body: Vec::new() }
    }

    fn u8(&mut self, value: u8) -> &mut Self {
        self.body.push(value);
        self
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.body.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// Write a length-prefixed name, encrypting it the way the container
    /// expects to find it.
    fn string(&mut self, text: &str, keys: (u32, u32), unicode: bool) -> &mut Self {
        let plain: Vec<u8> = if unicode {
            text.encode_utf16().flat_map(u16::to_le_bytes).collect()
        } else {
            text.as_bytes().to_vec()
        };
        let length = if unicode {
            (plain.len() / 2) as u32
        } else {
            plain.len() as u32
        };
        let seed = length.wrapping_add(keys.1);
        let cipher = xor(&plain, seed, self.version.is_ea06());
        self.u32(length ^ keys.0);
        self.body.extend_from_slice(&cipher);
        self
    }

    /// Write the placeholder record the interpreter is told to skip.
    ///
    /// It has the usual header but no payload: a byte, then a length that
    /// says how much to step over — `length + 0x18` bytes.
    fn placeholder(&mut self, name: &str, length: u32) -> &mut Self {
        let keys = self.version.keys();
        let ea06 = self.version.is_ea06();
        self.body.extend_from_slice(&xor(FILE_MAGIC, keys.res_type, ea06));
        self.string(SUBTYPE_NO_CMDEXECUTE, keys.res_sub_type, keys.unicode);
        self.string(name, keys.res_name, keys.unicode);
        self.u8(0);
        self.u32(length ^ keys.res_size);
        self.body.extend(vec![0u8; length as usize + 0x18]);
        self
    }

    /// Write one normal record.
    fn file(&mut self, sub_type: &str, name: &str, data: &[u8], compress: bool) -> &mut Self {
        let keys = self.version.keys();
        let ea06 = self.version.is_ea06();
        let stored = if compress {
            compress_for_test(data, ea06)
        } else {
            data.to_vec()
        };

        self.body.extend_from_slice(&xor(FILE_MAGIC, keys.res_type, ea06));
        self.string(sub_type, keys.res_sub_type, keys.unicode);
        self.string(name, keys.res_name, keys.unicode);
        self.u8(u8::from(compress));
        self.u32((stored.len() as u32) ^ keys.res_size);
        self.u32((data.len() as u32) ^ keys.res_size);
        self.u32(adler32(&stored) ^ keys.res_crc);
        // Two FILETIMEs; the reader skips them.
        self.body.extend_from_slice(&[0u8; 16]);
        let seed = 0u32.wrapping_add(keys.res_content);
        self.body.extend_from_slice(&xor(&stored, seed, ea06));
        self
    }

    /// The whole chunk, with the version signature and the header the
    /// reader expects in front.
    fn chunk(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // The reader is handed the bytes *after* the version signature.
        out.extend_from_slice(b"AU3!");
        out.extend_from_slice(self.version.signature());
        out.extend_from_slice(&[0u8; 16]); // the ignored header
        out.extend_from_slice(&self.body);
        out
    }

    /// A chunk with the salt and marker in front, as it sits in an image.
    fn image(&self) -> Vec<u8> {
        let mut out = vec![0xAAu8; 16]; // per-build salt
        out.extend_from_slice(&self.chunk());
        out
    }
}

/// A byte-for-byte valid *literal-only* compressed blob.
///
/// The real compressor is not reproduced here (the tests read blobs it
/// produced through the env-gated sample instead); this is enough to prove
/// the record layer hands compressed bodies to the right decoder.
fn compress_for_test(data: &[u8], ea06: bool) -> Vec<u8> {
    let mut stream: Vec<u8> = Vec::new();
    let mut position = 0usize;
    let literal = u32::from(ea06);
    for byte in data {
        for (width, value) in [(1usize, literal), (8usize, u32::from(*byte))] {
            for i in (0..width).rev() {
                if position % 8 == 0 {
                    stream.push(0);
                }
                let last = stream.len() - 1;
                stream[last] |= (((value >> i) & 1) as u8) << (7 - position % 8);
                position += 1;
            }
        }
    }
    let mut out = if ea06 { b"EA06".to_vec() } else { b"EA05".to_vec() };
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(&stream);
    out
}

#[test]
fn a_plain_utf16_script_round_trips() {
    let text = "MsgBox(0, \"hi\", \"there\")\r\n";
    // A `>AUTOIT UNICODE SCRIPT<` record's body really is UTF-16.
    let body: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut bundle = Bundle::new(ScriptVersion::Ea06);
    bundle.file(SUBTYPE_UNICODE, "C:\\tmp\\a.au3", &body, false);
    let compiled = from_bytes(&bundle.image()).unwrap();
    assert_eq!(compiled.version, ScriptVersion::Ea06);
    assert_eq!(compiled.files.len(), 1);
    assert_eq!(compiled.files[0].kind(), FileKind::UnicodeScript);
    assert_eq!(compiled.source().unwrap(), text);
}

#[test]
fn a_compressed_record_is_decompressed() {
    let mut bundle = Bundle::new(ScriptVersion::Ea06);
    bundle.file(SUBTYPE_SOURCE, "a.au3", b"ConsoleWrite(\"packed\")\r\n", true);
    let compiled = from_bytes(&bundle.image()).unwrap();
    assert_eq!(compiled.files[0].data, b"ConsoleWrite(\"packed\")\r\n");
    assert_eq!(compiled.source().unwrap(), "ConsoleWrite(\"packed\")\r\n");
}

#[test]
fn the_ea05_layout_reads_too() {
    let mut bundle = Bundle::new(ScriptVersion::Ea05);
    bundle.file(SUBTYPE_SOURCE, "C:\\a.au3", b"Exit\r\n", false);
    let compiled = from_bytes(&bundle.image()).unwrap();
    assert_eq!(compiled.version, ScriptVersion::Ea05);
    assert_eq!(compiled.source().unwrap(), "Exit\r\n");
}

#[test]
fn the_no_cmdexecute_placeholder_is_stepped_over() {
    // The sample's first record is this placeholder; if the step is off by
    // one byte the script record after it is never reached.
    let mut bundle = Bundle::new(ScriptVersion::Ea06);
    bundle.placeholder("C:\\tmp\\aut7190.tmp", 40);
    bundle.file(SUBTYPE_SOURCE, "C:\\tmp\\a.au3", b"Exit\r\n", false);
    let compiled = from_bytes(&bundle.image()).unwrap();
    assert_eq!(compiled.files.len(), 1, "the placeholder is not a file");
    assert_eq!(compiled.source().unwrap(), "Exit\r\n");
}

#[test]
fn a_corrupt_body_is_rejected_by_its_checksum() {
    let mut bundle = Bundle::new(ScriptVersion::Ea06);
    bundle.file(SUBTYPE_SOURCE, "a.au3", b"Exit\r\n", false);
    let mut image = bundle.image();
    // Flip a byte near the end, which is inside the record's ciphertext.
    let last = image.len() - 1;
    image[last] ^= 0x01;
    assert!(matches!(from_bytes(&image), Err(Error::BadData(_))));
}

#[test]
fn a_payload_only_build_has_no_script() {
    let mut bundle = Bundle::new(ScriptVersion::Ea06);
    bundle.file(">AUTOIT SOMETHING ELSE<", "data.bin", &[1, 2, 3], false);
    let compiled = from_bytes(&bundle.image()).unwrap();
    assert!(compiled.script().is_none());
    assert!(matches!(compiled.source(), Err(Error::NoScript)));
}

#[test]
fn no_marker_means_no_script() {
    assert!(matches!(from_bytes(&[0u8; 64]), Err(Error::NoCompiledScript)));
}

#[test]
fn adler32_matches_the_known_value() {
    // `zlib.adler32(b"Wikipedia")` is 0x11E60398.
    assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
}

/// A real build, when one is available.
///
/// `AU3_UNPACK_SCRIPT` may point at the `.exe` — or at a chunk already
/// dumped out of it. When `AU3_UNPACK_EXPECTED` names the `.au3` the build
/// was compiled from, the extracted text is compared byte for byte; that
/// is the check that says the whole chain (locate, decrypt, decompress,
/// deassemble) is faithful, and it is why the test is not merely
/// "something came out".
#[test]
fn a_real_build_is_read_back_byte_for_byte() {
    let Some(path) = std::env::var("AU3_UNPACK_SCRIPT").ok().filter(|p| !p.is_empty()) else {
        return;
    };
    let compiled = from_image(&path).expect("the build holds a compiled script");
    let source = compiled.source().expect("the build carries a script");

    let expected = std::env::var("AU3_UNPACK_EXPECTED").ok().filter(|p| !p.is_empty());
    if let Some(expected_path) = expected {
        let want = std::fs::read_to_string(&expected_path).expect("the expected source is readable");
        assert_eq!(source, want, "the extracted source differs from {expected_path}");
    } else {
        assert!(!source.is_empty(), "the build decoded to nothing");
    }
}
