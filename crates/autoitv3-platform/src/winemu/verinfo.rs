//! PE version resources — what `FileGetVersion` reads.
//!
//! The data lives in the image's `RT_VERSION` resource (type `16`), a tree of
//! `VS_VERSION_INFO` blocks: a root block holding a `VS_FIXEDFILEINFO`, and a
//! `StringFileInfo` child holding `(language, codepage)` blocks whose children
//! are the named string fields. Only what `FileGetVersion` needs is decoded.
//!
//! # Deliberate approximations
//!
//! * Only the first language/codepage block of `StringFileInfo` is kept, so a
//!   `"080904b0\Comments"` selector is not honoured — the plain field name is.
//! * `VarFileInfo` is skipped entirely.

use super::pe::{PeImage, Selector};

/// The resource type id of `RT_VERSION`.
const RT_VERSION: u32 = 16;

/// The version fields found in a PE's `RT_VERSION` resource.
#[derive(Debug, Clone, Default)]
pub struct VersionInfo {
    /// `VS_FIXEDFILEINFO.dwFileVersion` as `(major, minor, build, revision)`.
    pub fixed: Option<(u16, u16, u16, u16)>,
    /// `StringFileInfo` entries, in the order they appear.
    pub strings: Vec<(String, String)>,
}

impl VersionInfo {
    /// The value of a string field, matched case-insensitively.
    pub fn string(&self, name: &str) -> Option<&str> {
        self.strings
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// `"#.#.#.#"` from the fixed version, or `None` when there is none.
    pub fn dotted(&self) -> Option<String> {
        self.fixed
            .map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}"))
    }
}

/// Read the version resource of the PE file at `path`.
pub fn read(path: &str) -> Option<VersionInfo> {
    let image = PeImage::load(path).ok()?;
    let resource = image.find(&Selector::id(1), &Selector::id(RT_VERSION))?;
    parse(&resource.data)
}

/// One `VS_VERSION_INFO` block: its key, raw value and children.
struct Block<'a> {
    key: String,
    value: &'a [u8],
    /// `wType == 1`: the value is a UTF-16 string rather than binary.
    value_is_string: bool,
    children: Vec<Block<'a>>,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn parse(data: &[u8]) -> Option<VersionInfo> {
    let (root, _) = parse_block(data, 0)?;
    let mut info = VersionInfo::default();

    // The root's value is a VS_FIXEDFILEINFO: dwFileVersionMS at 8, LS at 12.
    if root.value.len() >= 16 {
        let ms = u32_at(root.value, 8)?;
        let ls = u32_at(root.value, 12)?;
        info.fixed = Some((
            (ms >> 16) as u16,
            ms as u16,
            (ls >> 16) as u16,
            ls as u16,
        ));
    }

    for child in &root.children {
        if !child.key.eq_ignore_ascii_case("StringFileInfo") {
            continue;
        }
        for lang in &child.children {
            for field in &lang.children {
                if field.value_is_string {
                    info.strings
                        .push((field.key.clone(), decode_utf16(field.value)));
                }
            }
        }
    }
    Some(info)
}

/// Parse one block starting at `off`; returns it and the offset just past it.
fn parse_block(b: &[u8], off: usize) -> Option<(Block<'_>, usize)> {
    let w_len = u16_at(b, off)? as usize;
    let w_value_len = u16_at(b, off + 2)? as usize;
    let w_type = u16_at(b, off + 4)?;
    let end = (off + w_len).min(b.len());

    // UTF-16, null-terminated key.
    let mut p = off + 6;
    let mut key = String::new();
    while p + 1 < end {
        let c = u16::from_le_bytes([b[p], b[p + 1]]);
        p += 2;
        if c == 0 {
            break;
        }
        key.push(char::from_u32(u32::from(c)).unwrap_or('\u{fffd}'));
    }
    p = align4(p);

    // The value length counts characters for a string, bytes otherwise.
    let value_len = if w_type == 1 {
        w_value_len * 2
    } else {
        w_value_len
    };
    let value = b.get(p..(p + value_len).min(end)).unwrap_or(&[]);
    let mut child_off = align4(p + value_len);

    let mut children = Vec::new();
    while child_off + 6 <= end {
        let (child, next) = parse_block(b, child_off)?;
        if next <= child_off {
            break;
        }
        children.push(child);
        child_off = align4(next);
    }

    Some((
        Block {
            key,
            value,
            value_is_string: w_type == 1,
            children,
        },
        end,
    ))
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Decode UTF-16LE bytes up to the first NUL.
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/winemu_verinfo.rs"]
mod tests;
