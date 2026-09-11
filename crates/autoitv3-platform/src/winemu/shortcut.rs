//! Shell Links (`.lnk`) — what `FileCreateShortcut` writes and
//! `FileGetShortcut` reads.
//!
//! The on-disk format is Microsoft's Shell Link Binary File Format: a 76-byte
//! header, an optional target id-list, an optional `LinkInfo` block carrying the
//! local path, and the `StringData` fields (description, relative path, working
//! directory, arguments, icon location). This module writes and reads the subset
//! AutoIt's two functions expose.
//!
//! # Deliberate approximations
//!
//! * `LinkTargetIDList` is not written; the target is carried in `LinkInfo` and
//!   `RELATIVE_PATH`, which is how most shortcuts resolve anyway. A foreign
//!   shortcut's id-list is skipped when reading.
//! * Paths are stored ANSI (the format's "local base path"), so non-ASCII
//!   targets are not round-tripped exactly.

use std::io;

/// The fields a shortcut carries.
#[derive(Debug, Clone, Default)]
pub struct Shortcut {
    /// The file or program the shortcut points at.
    pub target: String,
    /// The directory the target is launched in.
    pub working_dir: String,
    /// Command-line arguments.
    pub arguments: String,
    /// Tooltip text.
    pub description: String,
    /// File holding the icon.
    pub icon_location: String,
    /// Index of the icon within `icon_location`.
    pub icon_index: i32,
    /// `@SW_SHOWNORMAL` (1), `@SW_SHOWMINNOACTIVE` (7) or
    /// `@SW_SHOWMAXIMIZED` (3).
    pub show_command: u32,
    /// Packed hotkey (`^`, `!`, `+` flags in the high byte, VK in the low).
    pub hotkey: u16,
}

const HEADER_SIZE: u32 = 0x4C;
/// `{00021401-0000-0000-C000-000000000046}`.
const LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x46,
];
const HAS_TARGET_ID_LIST: u32 = 0x0000_0001;
const HAS_LINK_INFO: u32 = 0x0000_0002;
const HAS_NAME: u32 = 0x0000_0004;
const HAS_RELATIVE_PATH: u32 = 0x0000_0008;
const HAS_WORKING_DIR: u32 = 0x0000_0010;
const HAS_ARGUMENTS: u32 = 0x0000_0020;
const HAS_ICON_LOCATION: u32 = 0x0000_0040;
const IS_UNICODE: u32 = 0x0000_0080;

/// Write `sc` to `path`.
pub fn write(path: &str, sc: &Shortcut) -> io::Result<()> {
    std::fs::write(path, encode(sc))
}

/// Read the shortcut at `path`, or `None` when it is not a shell link.
pub fn read(path: &str) -> Option<Shortcut> {
    let bytes = std::fs::read(path).ok()?;
    decode(&bytes)
}

fn encode(sc: &Shortcut) -> Vec<u8> {
    let mut flags = IS_UNICODE;
    if !sc.target.is_empty() {
        flags |= HAS_LINK_INFO | HAS_RELATIVE_PATH;
    }
    if !sc.description.is_empty() {
        flags |= HAS_NAME;
    }
    if !sc.working_dir.is_empty() {
        flags |= HAS_WORKING_DIR;
    }
    if !sc.arguments.is_empty() {
        flags |= HAS_ARGUMENTS;
    }
    if !sc.icon_location.is_empty() {
        flags |= HAS_ICON_LOCATION;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&LINK_CLSID);
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // FileAttributes
    out.extend_from_slice(&[0u8; 24]); // Creation/Access/Write FILETIMEs
    out.extend_from_slice(&0u32.to_le_bytes()); // FileSize
    out.extend_from_slice(&sc.icon_index.to_le_bytes());
    out.extend_from_slice(&sc.show_command.to_le_bytes());
    out.extend_from_slice(&sc.hotkey.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // Reserved1
    out.extend_from_slice(&0u32.to_le_bytes()); // Reserved2
    out.extend_from_slice(&0u32.to_le_bytes()); // Reserved3
    debug_assert_eq!(out.len(), 76);

    if !sc.target.is_empty() {
        out.extend_from_slice(&encode_link_info(&sc.target));
    }
    // StringData, in the order the flags are read back.
    if !sc.description.is_empty() {
        out.extend_from_slice(&string_data(&sc.description));
    }
    if !sc.target.is_empty() {
        out.extend_from_slice(&string_data(&sc.target));
    }
    if !sc.working_dir.is_empty() {
        out.extend_from_slice(&string_data(&sc.working_dir));
    }
    if !sc.arguments.is_empty() {
        out.extend_from_slice(&string_data(&sc.arguments));
    }
    if !sc.icon_location.is_empty() {
        out.extend_from_slice(&string_data(&sc.icon_location));
    }
    out
}

fn string_data(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::with_capacity(2 + units.len() * 2);
    out.extend_from_slice(&(units.len() as u16).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// A `LinkInfo` block whose `LocalBasePath` is the target.
fn encode_link_info(target: &str) -> Vec<u8> {
    const HEADER: u32 = 0x1C;
    let label = b"C:\\";
    let volume_id_size = 16 + label.len() + 1;
    let local_path = target.as_bytes();
    let volume_off = HEADER as usize;
    let local_off = volume_off + volume_id_size;
    let suffix_off = local_off + local_path.len() + 1;
    let size = suffix_off + 1;

    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&HEADER.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
    out.extend_from_slice(&(volume_off as u32).to_le_bytes());
    out.extend_from_slice(&(local_off as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // CommonNetworkRelativeLinkOffset
    out.extend_from_slice(&(suffix_off as u32).to_le_bytes());
    // VolumeID: size, DRIVE_FIXED, serial, label offset, ANSI label.
    out.extend_from_slice(&(volume_id_size as u32).to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(HEADER).to_le_bytes());
    out.extend_from_slice(label);
    out.push(0);
    // LocalBasePath, then CommonPathSuffix.
    out.extend_from_slice(local_path);
    out.push(0);
    out.push(0);
    out
}

fn decode(b: &[u8]) -> Option<Shortcut> {
    if u32_at(b, 0)? != HEADER_SIZE {
        return None;
    }
    let flags = u32_at(b, 20)?;
    let icon_index = i32::from_le_bytes(b.get(56..60)?.try_into().ok()?);
    let show_command = u32_at(b, 60)?;
    let hotkey = u16_at(b, 64)?;
    let mut sc = Shortcut {
        icon_index,
        show_command,
        hotkey,
        ..Shortcut::default()
    };

    let mut off = 76;
    if flags & HAS_TARGET_ID_LIST != 0 {
        let size = u16_at(b, off)? as usize;
        off += 2 + size;
    }
    if flags & HAS_LINK_INFO != 0 {
        let link_info_size = u32_at(b, off)? as usize;
        let header_size = u32_at(b, off + 4)? as usize;
        let info_flags = u32_at(b, off + 8)?;
        let local_off = u32_at(b, off + 16)? as usize;
        if info_flags & 1 != 0 && header_size >= 0x1C && local_off != 0 {
            sc.target = ansi_z(b, off + local_off);
        }
        off += link_info_size;
    }

    let take = |off: &mut usize| -> Option<String> {
        let s = read_string(b, *off)?;
        *off += 2 + s.encode_utf16().count() * 2;
        Some(s)
    };
    if flags & HAS_NAME != 0 {
        sc.description = take(&mut off)?;
    }
    if flags & HAS_RELATIVE_PATH != 0 {
        let relative = take(&mut off)?;
        if sc.target.is_empty() {
            sc.target = relative;
        }
    }
    if flags & HAS_WORKING_DIR != 0 {
        sc.working_dir = take(&mut off)?;
    }
    if flags & HAS_ARGUMENTS != 0 {
        sc.arguments = take(&mut off)?;
    }
    if flags & HAS_ICON_LOCATION != 0 {
        sc.icon_location = take(&mut off)?;
    }
    Some(sc)
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// A UTF-16 string whose length prefix counts characters.
fn read_string(b: &[u8], off: usize) -> Option<String> {
    let count = u16_at(b, off)? as usize;
    let bytes = b.get(off + 2..off + 2 + count * 2)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// A NUL-terminated ANSI string.
fn ansi_z(b: &[u8], off: usize) -> String {
    let end = b[off.min(b.len())..]
        .iter()
        .position(|c| *c == 0)
        .map(|i| off + i)
        .unwrap_or(b.len());
    b.get(off..end)
        .map(|s| s.iter().map(|c| *c as char).collect())
        .unwrap_or_default()
}
