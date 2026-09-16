//! Reading the script an AutoIt build carries inside it.
//!
//! `aut2exe` embeds the script — the source, tokenised and compressed — in the
//! executable it produces. The chunk is self-describing, so it can be read
//! back without running anything, which is what this module does: locate the
//! chunk, decrypt it, decompress it, deassemble it and hand back the `.au3`
//! text the build was compiled from.
//!
//! The container is a small file system. After a version signature comes a
//! sequence of records, each starting with the literal `FILE`, carrying a
//! sub-type, the build-time path of the file, sizes, a CRC, two FILETIMEs and
//! the file's bytes — encrypted by XORing with a keystream keyed by constants
//! from the AutoIt source (see `keys.rs`). A build normally holds one
//! `>>>AUTOIT SCRIPT<<<` record; a second `>>>AUTOIT NO CMDEXECUTE<<<` record
//! is a placeholder the interpreter skips, and `FileInstall` payloads are
//! further records of their own.
//!
//! Two signatures exist in the wild:
//!
//! * `EA06` (AutoIt v3.2.0 and later) — UTF-16 names, the `LAME` keystream,
//!   and typically the whole chunk inside an `RT_RCDATA` resource literally
//!   named `SCRIPT`;
//! * `EA05` (earlier) — single-byte names, a Mersenne Twister keystream, and
//!   the chunk appended to the image rather than resource-homed.
//!
//! The `EA06` chunk is preceded by sixteen bytes of per-build salt that the
//! format does not use here; the marker `AU3!EA06` is what is searched for, so
//! a chunk found in a resource, in a section, or already dumped to a file all
//! read the same way.
//!
//! Everything below is a port of the MIT-licensed AutoIt-Ripper's
//! `autoit_unpack.py`. Its `AUTOIT NO CMDEXECUTE` handling and its habit of
//! reading hex/UTF-16 names are kept; the checks it performs (the Adler-32 of
//! each record, the signature of each compressed blob) are kept too, because a
//! wrongly-located chunk should fail loudly rather than yield noise.

mod keys;
mod lz;
pub mod symbols;
pub mod tokens;

use std::fmt;
use std::path::Path;

use autoitv3_i18n::{msg, tr};
use autoitv3_platform::PeImage;

use crate::Error;

/// The literal every record starts with.
const FILE_MAGIC: &[u8; 4] = b"FILE";

/// The sub-type of a record holding a compiled (tokenised) script.
const SUBTYPE_COMPILED: &str = ">>>AUTOIT SCRIPT<<<";
/// The sub-type of a record holding plain source as UTF-16.
const SUBTYPE_UNICODE: &str = ">AUTOIT UNICODE SCRIPT<";
/// The sub-type of a record holding plain source in the image's code page.
const SUBTYPE_SOURCE: &str = ">AUTOIT SCRIPT<";
/// A record the interpreter is told not to run; it has no data of its own.
const SUBTYPE_NO_CMDEXECUTE: &str = ">>>AUTOIT NO CMDEXECUTE<<<";

/// The 8-byte marker that ends the `EA06` salt and starts the container.
const MARKER_EA06: &[u8; 8] = b"AU3!EA06";
/// The same for `EA05`.
const MARKER_EA05: &[u8; 8] = b"AU3!EA05";

/// Which flavour of compiled script this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptVersion {
    /// AutoIt v3.2.0 and later: UTF-16 names, `LAME` keystream.
    Ea06,
    /// Earlier AutoIt v3: single-byte names, Mersenne Twister keystream.
    Ea05,
}

impl ScriptVersion {
    /// The signature bytes, `EA05` or `EA06`.
    pub fn signature(self) -> &'static [u8; 4] {
        match self {
            ScriptVersion::Ea05 => b"EA05",
            ScriptVersion::Ea06 => b"EA06",
        }
    }

    /// The keys the container's fields are encrypted with.
    ///
    /// Each field is XORed with a keystream seeded from one of these numbers,
    /// with the string fields adding their own length to the seed first. They
    /// are constants of the AutoIt build, not per-file data.
    fn keys(self) -> Keys {
        match self {
            ScriptVersion::Ea05 => Keys {
                unicode: false,
                res_type: 0x16FA,
                res_sub_type: (0x29BC, 0xA25E),
                res_name: (0x29AC, 0xF25E),
                res_size: 0x45AA,
                res_crc: 0xC3D2,
                res_content: 0x22AF,
            },
            ScriptVersion::Ea06 => Keys {
                unicode: true,
                res_type: 0x18EE,
                res_sub_type: (0xADBC, 0xB33F),
                res_name: (0xF820, 0xF479),
                res_size: 0x87BC,
                res_crc: 0xA685,
                res_content: 0x2477,
            },
        }
    }

    /// Whether the keystream is `EA06`'s rather than `EA05`'s.
    fn is_ea06(self) -> bool {
        matches!(self, ScriptVersion::Ea06)
    }
}

impl fmt::Display for ScriptVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScriptVersion::Ea05 => "EA05",
            ScriptVersion::Ea06 => "EA06",
        })
    }
}

/// The per-version encryption constants.
struct Keys {
    unicode: bool,
    res_type: u32,
    /// `(length XOR key, seed offset)` for the sub-type string.
    res_sub_type: (u32, u32),
    /// The same for the recorded path.
    res_name: (u32, u32),
    res_size: u32,
    res_crc: u32,
    res_content: u32,
}

/// What an embedded record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// A tokenised script, to be deassembled into source.
    CompiledScript,
    /// Source stored as UTF-16.
    UnicodeScript,
    /// Source stored in the image's code page.
    Script,
    /// Something else a build embedded, such as a `FileInstall` payload.
    Other,
}

/// One record read back out of a build.
#[derive(Debug, Clone)]
pub struct ScriptFile {
    /// The container's own label, e.g. `>>>AUTOIT SCRIPT<<<`.
    pub sub_type: String,
    /// The path the build recorded for it, usually a temporary file name.
    pub name: String,
    /// The file's bytes, after decryption and decompression.
    pub data: Vec<u8>,
}

impl ScriptFile {
    /// Classify the record by its sub-type.
    pub fn kind(&self) -> FileKind {
        match self.sub_type.as_str() {
            SUBTYPE_COMPILED => FileKind::CompiledScript,
            SUBTYPE_UNICODE => FileKind::UnicodeScript,
            SUBTYPE_SOURCE => FileKind::Script,
            _ => FileKind::Other,
        }
    }

    /// The record's text, if it is a script: `None` for payloads.
    ///
    /// A tokenised script is deassembled; a UTF-16 script is decoded; a plain
    /// script is read as UTF-8, replacing anything that is not (AutoIt's own
    /// source is ASCII in practice).
    pub fn source(&self) -> Result<Option<String>, Error> {
        match self.kind() {
            FileKind::CompiledScript => tokens::deassemble(&self.data)
                .map(Some)
                .map_err(|e| Error::BadData(msg!("the token stream could not be read: {e}", e = e))),
            FileKind::UnicodeScript => Ok(Some(decode_utf16(&self.data)?)),
            FileKind::Script => Ok(Some(String::from_utf8_lossy(&self.data).into_owned())),
            FileKind::Other => Ok(None),
        }
    }
}

/// A decoded build: the version it was made with and its embedded files.
#[derive(Debug, Clone)]
pub struct CompiledScript {
    /// Which container format the chunk used.
    pub version: ScriptVersion,
    /// Every record, in the order the compiler wrote them.
    pub files: Vec<ScriptFile>,
}

impl CompiledScript {
    /// The first record that is a script.
    ///
    /// A build has one; the accessor exists because the container itself does
    /// not say which record it is, only the sub-type does.
    pub fn script(&self) -> Option<&ScriptFile> {
        self.files.iter().find(|f| f.kind() != FileKind::Other)
    }

    /// The script source this build was compiled from.
    pub fn source(&self) -> Result<String, Error> {
        let file = self.script().ok_or(Error::NoScript)?;
        file.source()?.ok_or(Error::NoScript)
    }
}

// ---------------------------------------------------------------------------
// Finding and reading the chunk
// ---------------------------------------------------------------------------

/// Read the compiled script inside a PE image, or inside a raw chunk.
///
/// The `RT_RCDATA` resource named `SCRIPT` is tried first, because that is
/// where `AutoIt3Wrapper` leaves it and where a name saves guessing; anything
/// else — the chunk appended to a section, an extracted `.a3x`, a resource
/// under another name — is found by scanning for the `AU3!EA05`/`AU3!EA06`
/// marker. Every candidate is parsed to the end and must pass its CRC, so a
/// coincidental marker does not win over the real chunk.
pub fn from_image(path: impl AsRef<Path>) -> Result<CompiledScript, Error> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;

    if let Ok(image) = PeImage::load(path) {
        for resource in &image.resources {
            if !is_rcdata(resource.type_sel.id, resource.type_sel.name.as_deref()) {
                continue;
            }
            let is_script = resource
                .name_sel
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("SCRIPT"));
            if is_script {
                if let Ok(compiled) = from_bytes(&resource.data) {
                    return Ok(compiled);
                }
            }
        }
    }
    from_bytes(&bytes)
}

/// Read the compiled script out of bytes already in hand.
///
/// The bytes may be a whole image, a resource, or a bare chunk; only the
/// marker inside matters.
pub fn from_bytes(image: &[u8]) -> Result<CompiledScript, Error> {
    let candidates = markers(image);
    if candidates.is_empty() {
        return Err(Error::NoCompiledScript);
    }
    let mut first_error = None;
    for (position, version) in candidates {
        // `position` is the start of the marker; the container's own records
        // begin eight bytes later, after `AU3!EAxx`.
        match parse_records(&image[position + 8..], version) {
            Ok(compiled) => return Ok(compiled),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    // Markers existed, so the file is the right shape but the chunk is not
    // readable; say why rather than pretending nothing was found.
    Err(first_error.unwrap_or(Error::NoCompiledScript))
}

/// Every `AU3!EAxx` marker in `image`, earliest first.
fn markers(image: &[u8]) -> Vec<(usize, ScriptVersion)> {
    let mut found = Vec::new();
    for (marker, version) in [
        (MARKER_EA05, ScriptVersion::Ea05),
        (MARKER_EA06, ScriptVersion::Ea06),
    ] {
        let mut from = 0;
        while let Some(offset) = find(&image[from..], marker) {
            let position = from + offset;
            found.push((position, version));
            from = position + 1;
        }
    }
    found.sort_by_key(|(position, _)| *position);
    found
}

/// The offset of the first `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether a resource type selector means `RT_RCDATA` (id `10`).
fn is_rcdata(id: Option<u32>, name: Option<&str>) -> bool {
    id == Some(10) || name.is_some_and(|name| name.eq_ignore_ascii_case("RCDATA"))
}

/// Read the records of a chunk, starting just after its version signature.
///
/// The first sixteen bytes are consumed and, for `EA05`, summed into the seed
/// the record bodies are decrypted with; `EA06` ignores the value but the
/// bytes are still part of the layout.
fn parse_records(rest: &[u8], version: ScriptVersion) -> Result<CompiledScript, Error> {
    let keys = version.keys();
    let ea06 = version.is_ea06();
    let mut reader = Reader::new(rest);

    let seed = if ea06 {
        reader.skip(16)?;
        0
    } else {
        reader.take(16)?.iter().map(|byte| u32::from(*byte)).sum()
    };

    let mut files = Vec::new();
    loop {
        // The container has no length field: the end is "the next record does
        // not start with FILE", which includes running out of bytes.
        if reader.remaining() < 4 {
            break;
        }
        let magic = reader.decrypt(4, keys.res_type, ea06)?;
        if magic != FILE_MAGIC {
            break;
        }
        let sub_type = reader.read_string(keys.res_sub_type, keys.unicode, ea06)?;
        let name = reader.read_string(keys.res_name, keys.unicode, ea06)?;

        if sub_type == SUBTYPE_NO_CMDEXECUTE {
            // A placeholder with no payload; its own size says how much to
            // step over so the next record stays aligned.
            reader.skip(1)?;
            let length = reader.u32()? ^ keys.res_size;
            reader.skip(length as usize + 0x18)?;
            continue;
        }

        let is_compressed = reader.u8()?;
        let compressed_size = reader.u32()? ^ keys.res_size;
        let _uncompressed_size = reader.u32()? ^ keys.res_size;
        let checksum = reader.u32()? ^ keys.res_crc;
        // Two FILETIMEs (creation and last write), of no use on a file that
        // has no index.
        reader.skip(16)?;

        let ciphertext = reader.take(compressed_size as usize)?;
        let mut data = xor(ciphertext, seed.wrapping_add(keys.res_content), ea06);
        // The Adler-32 is the format's only integrity check, and it is what
        // rejects a chunk that only looked like one.
        if adler32(&data) != checksum {
            let record = format!("{sub_type:?}");
            let checksum = format!("{checksum:#010x}");
            return Err(Error::BadData(msg!(
                "the {record} record fails its Adler-32 check (want {checksum})",
                record = record,
                checksum = checksum
            )));
        }
        if is_compressed == 1 {
            let record = format!("{sub_type:?}");
            data = lz::decompress(&data, ea06)
                .map_err(|e| Error::BadData(msg!(
                    "the {record} record is compressed oddly: {e}",
                    record = record,
                    e = e
                )))?;
        }
        files.push(ScriptFile { sub_type, name, data });
    }

    // A marker can sit in unrelated data; a chunk with no records at all is
    // not a chunk, and saying so keeps the scan from stopping on a coincidence.
    if files.is_empty() {
        return Err(Error::BadData(tr("no records follow the signature").into()));
    }
    Ok(CompiledScript { version, files })
}

/// A cursor that refuses to run off the end of the chunk.
struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| Error::BadData(tr("a record is longer than the chunk").into()))?;
        let slice = self.data.get(self.position..end).ok_or_else(|| {
            Error::BadData(tr("the chunk ends in the middle of a record").into())
        })?;
        self.position = end;
        Ok(slice)
    }

    /// How many bytes are left, for the "is this the end?" test.
    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    fn skip(&mut self, count: usize) -> Result<(), Error> {
        self.take(count).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Four bytes, decrypted, as a plain comparison value.
    fn decrypt(&mut self, count: usize, seed: u32, ea06: bool) -> Result<Vec<u8>, Error> {
        let slice = self.take(count)?;
        Ok(xor(slice, seed, ea06))
    }

    /// A length-prefixed, XOR-obfuscated name.
    ///
    /// The length is a character count; the keystream seed is the length plus
    /// a constant, so a wrong length yields bytes that do not decode rather
    /// than a wrong string.
    fn read_string(
        &mut self,
        keys: (u32, u32),
        unicode: bool,
        ea06: bool,
    ) -> Result<String, Error> {
        let length = self.u32()? ^ keys.0;
        let seed = length.wrapping_add(keys.1);
        let bytes = if unicode {
            (length as usize).saturating_mul(2)
        } else {
            length as usize
        };
        let ciphertext = self.take(bytes)?;
        let plain = xor(ciphertext, seed, ea06);
        if unicode {
            decode_utf16(&plain)
        } else {
            Ok(String::from_utf8_lossy(&plain).into_owned())
        }
    }
}

/// XOR `data` with the keystream `seed` selects.
fn xor(data: &[u8], seed: u32, ea06: bool) -> Vec<u8> {
    keys::xor_keystream(data, seed, ea06)
}

/// Decode UTF-16 little-endian, which is what AutoIt writes.
fn decode_utf16(data: &[u8]) -> Result<String, Error> {
    if data.len() % 2 != 0 {
        return Err(Error::BadData(tr("a UTF-16 string has an odd byte count").into()));
    }
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| Error::BadData(tr("a UTF-16 string is malformed").into()))
}

/// The Adler-32 check the container stores for every record.
fn adler32(data: &[u8]) -> u32 {
    const MODULUS: u32 = 65521;
    let mut low = 1u32;
    let mut high = 0u32;
    for byte in data {
        low = (low + u32::from(*byte)) % MODULUS;
        high = (high + low) % MODULUS;
    }
    (high << 16) | low
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/script_container.rs"]
mod tests;
