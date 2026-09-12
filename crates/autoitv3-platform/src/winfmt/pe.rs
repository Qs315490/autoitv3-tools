//! Reading a PE file's resource directory.
//!
//! Obfuscated AutoIt scripts ship their payload as an embedded resource — a
//! sample's build script has lines like
//!
//! ```text
//! #AutoIt3Wrapper_Res_File_Add=__PAYLOAD, RT_RCDATA, PAYLOAD, 0
//! ```
//!
//! and the script walks `GetModuleHandleW` → `FindResourceW` →
//! `SizeofResource` → `LoadResource` → `LockResource` to get at the bytes. Off
//! Windows there is no module to ask, so this module reads them straight out of
//! a real PE file: point the emulation at the `.exe` the script was compiled
//! from (see `WindowsEmulation::with_module_file`) and those calls are answered
//! from its resource section.
//!
//! Only what a lookup needs is parsed: the DOS/NT headers, the section table
//! (for RVA → file offset) and the three-level resource tree
//! (type → name → language). Nothing is executed and no section is mapped.

/// A `FindResourceW` selector: either an integer id or a string name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    /// The numeric form, for `MAKEINTRESOURCE`-style lookups.
    pub id: Option<u32>,
    /// The string form.
    pub name: Option<String>,
}

impl Selector {
    /// An integer selector.
    pub fn id(id: u32) -> Self {
        Self {
            id: Some(id),
            name: None,
        }
    }

    /// A string selector.
    pub fn name(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: Some(name.into()),
        }
    }

    fn matches(&self, id: Option<u32>, name: Option<&str>) -> bool {
        match (&self.id, &self.name) {
            (Some(want), _) => id == Some(*want),
            (None, Some(want)) => name.is_some_and(|n| n.eq_ignore_ascii_case(want)),
            (None, None) => id.is_none() && name.is_none(),
        }
    }
}

/// One leaf of the resource tree.
#[derive(Debug, Clone)]
pub struct Resource {
    /// The type: `RT_RCDATA` is `10`.
    pub type_sel: Selector,
    /// The resource's name (or numeric id).
    pub name_sel: Selector,
    /// Language id; `0` for the neutral entry.
    pub language: u32,
    /// The resource bytes.
    pub data: Vec<u8>,
}

/// The resources of one PE image.
#[derive(Debug, Clone, Default)]
pub struct PeImage {
    /// The file the image was read from, for diagnostics.
    pub path: String,
    /// Every resource leaf, in directory order.
    pub resources: Vec<Resource>,
}

impl PeImage {
    /// Read the resources of the PE file at `path`.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let resources = parse(&bytes)?;
        Ok(Self {
            path: path.display().to_string(),
            resources,
        })
    }

    /// The first resource matching a `FindResourceW(name, type)` lookup.
    pub fn find(&self, name: &Selector, kind: &Selector) -> Option<&Resource> {
        // `FindResourceW` matches names case-insensitively, which matters
        // because a script's resource name comes from a string table.
        self.resources.iter().find(|r| {
            name.matches(r.name_sel.id, r.name_sel.name.as_deref())
                && kind.matches(r.type_sel.id, r.type_sel.name.as_deref())
        })
    }

    /// Every file in `dir` that parses as a PE image carrying resources, as
    /// `(path, image)` pairs sorted by name.
    fn images_in(dir: &std::path::Path) -> Vec<(std::path::PathBuf, PeImage)> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("dll"))
            })
            .collect();
        // Sorted so a directory holding several images resolves the same way
        // on every run.
        names.sort();
        names
            .into_iter()
            .filter_map(|p| PeImage::load(&p).ok().map(|img| (p, img)))
            .collect()
    }

    /// Pick the image in `dir` whose resources should answer `FindResourceW`.
    ///
    /// A script compiled into an `.exe` keeps its payload in that image's
    /// resources, so the common case needs no configuration: given the
    /// directory the script lives in, prefer an image named after the script,
    /// then the first PE image that actually carries resources.
    pub fn find_resource_module(
        dir: impl AsRef<std::path::Path>,
        stem: Option<&str>,
    ) -> Option<std::path::PathBuf> {
        let images = Self::images_in(dir.as_ref());
        if let Some(stem) = stem {
            if let Some((path, _)) = images.iter().find(|(p, _)| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(stem))
            }) {
                return Some(path.clone());
            }
        }
        images
            .into_iter()
            .find(|(_, img)| !img.resources.is_empty())
            .map(|(path, _)| path)
    }

    /// Read a resource out of a directory of files extracted from an image.
    ///
    /// `AutoIt3Wrapper_Res_File_Add` stages each embedded resource next to the
    /// script, and the staging path is recoverable from the resource name:
    /// `__NAME` for a file added straight from the directory, `__Res64/NAME`
    /// for the 64-bit payload directory and `__ResImage/_NAME` for the image
    /// one. Reading those files means an analysis does not need the (often
    /// multi-megabyte) `.exe` at all.
    ///
    /// `name` is matched case-insensitively, as `FindResourceW` does.
    pub fn find_resource_file(
        dirs: &[std::path::PathBuf],
        name: &Selector,
    ) -> Option<Vec<u8>> {
        let wanted = name.name.clone().or_else(|| name.id.map(|id| id.to_string()))?;
        for dir in dirs {
            for rel in [
                std::path::PathBuf::from(format!("__{wanted}")),
                std::path::PathBuf::from(&wanted),
                std::path::PathBuf::from("__Res64").join(&wanted),
                std::path::PathBuf::from("__ResImage").join(format!("_{wanted}")),
            ] {
                if let Some(path) = resolve_ci(&dir.join(rel)) {
                    if let Ok(bytes) = std::fs::read(&path) {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }

    /// `type/name` pairs, for diagnostics.
    pub fn listing(&self) -> Vec<String> {
        self.resources
            .iter()
            .map(|r| {
                format!(
                    "{}/{}",
                    describe(&r.type_sel),
                    describe(&r.name_sel)
                )
            })
            .collect()
    }
}

fn describe(sel: &Selector) -> String {
    sel.name
        .clone()
        .or_else(|| sel.id.map(|i| i.to_string()))
        .unwrap_or_else(|| "?".to_string())
}

/// Parse the resource directory out of a PE image.
fn parse(image: &[u8]) -> Result<Vec<Resource>, String> {
    if read_u16(image, 0) != Some(0x5a4d) {
        // "MZ"
        return Err("not a PE image (missing MZ)".to_string());
    }
    let nt = read_u32(image, 0x3c).ok_or("truncated DOS header")? as usize;
    if image.get(nt..nt + 4) != Some(b"PE\0\0") {
        return Err("not a PE image (missing PE signature)".to_string());
    }

    let coff = nt + 4;
    let sections = read_u16(image, coff + 2).ok_or("truncated COFF header")? as usize;
    let optional_size = read_u16(image, coff + 16).ok_or("truncated COFF header")? as usize;
    let optional = coff + 20;
    let magic = read_u16(image, optional).ok_or("truncated optional header")?;
    // The data directories follow the fixed part of the optional header: 96
    // bytes for PE32, 112 for PE32+. Directory 2 is the resource table.
    let dirs = optional
        + match magic {
            0x10b => 96,
            0x20b => 112,
            other => return Err(format!("unknown optional header magic {other:#x}")),
        };
    let rsrc_rva = read_u32(image, dirs + 2 * 8).ok_or("truncated data directories")?;
    if rsrc_rva == 0 || read_u32(image, dirs + 2 * 8 + 4).unwrap_or(0) == 0 {
        return Ok(Vec::new());
    }

    // Section table → RVA to file offset.
    let section_table = optional + optional_size;
    let mut sections_mapped = Vec::new();
    for i in 0..sections {
        let s = section_table + i * 40;
        let (Some(vsize), Some(vaddr), Some(raw_size), Some(raw)) = (
            read_u32(image, s + 8),
            read_u32(image, s + 12),
            read_u32(image, s + 16),
            read_u32(image, s + 20),
        ) else {
            break;
        };
        sections_mapped.push((vaddr, vsize.max(raw_size), raw));
    }
    let to_offset = |rva: u32| -> Option<usize> {
        let (vaddr, _vsize, raw) = sections_mapped
            .iter()
            .find(|(v, size, _)| rva >= *v && rva < v.saturating_add(*size))?;
        Some((raw + (rva - vaddr)) as usize)
    };

    let base = to_offset(rsrc_rva).ok_or("resource directory RVA is not mapped")?;
    let mut out = Vec::new();
    // Every offset inside the resource tree is relative to `base`, so it is
    // threaded through the recursion rather than added to the current node.
    walk(
        image,
        base,
        base,
        &to_offset,
        0,
        &Selector { id: None, name: None },
        &Selector { id: None, name: None },
        &mut out,
    );
    Ok(out)
}

/// Recursively walk the resource tree, collecting leaves.
///
/// Depth 0 is the type level, depth 1 the name level; a depth-2 entry is a
/// language pointing at an `IMAGE_RESOURCE_DATA_ENTRY`.
#[allow(clippy::too_many_arguments)]
fn walk(
    image: &[u8],
    base: usize,
    dir: usize,
    to_offset: &dyn Fn(u32) -> Option<usize>,
    depth: usize,
    kind: &Selector,
    name: &Selector,
    out: &mut Vec<Resource>,
) {
    let (Some(count), Some(named)) = (read_u16(image, dir + 12), read_u16(image, dir + 14)) else {
        return;
    };
    for i in 0..(count as usize + named as usize) {
        let entry = dir + 16 + i * 8;
        let (Some(id_field), Some(offset_field)) =
            (read_u32(image, entry), read_u32(image, entry + 4))
        else {
            return;
        };
        let has_name = id_field & 0x8000_0000 != 0;
        let string = has_name
            .then(|| read_name(image, base + (id_field & 0x7fff_ffff) as usize))
            .flatten();
        let (this_id, this_name) = if has_name {
            (None, string)
        } else {
            (Some(id_field), None)
        };

        if offset_field & 0x8000_0000 != 0 {
            // Subdirectory: descend with this level's selector.
            let child = base + (offset_field & 0x7fff_ffff) as usize;
            let (next_kind, next_name) = if depth == 0 {
                (
                    Selector {
                        id: this_id,
                        name: this_name.clone(),
                    },
                    name.clone(),
                )
            } else {
                (
                    kind.clone(),
                    Selector {
                        id: this_id,
                        name: this_name.clone(),
                    },
                )
            };
            walk(
                image, base, child, to_offset, depth + 1, &next_kind, &next_name, out,
            );
            continue;
        }

        // A leaf: the language entry points at the data.
        if depth < 2 {
            continue;
        }
        let Some(data_entry) = base.checked_add(offset_field as usize) else {
            continue;
        };
        let (Some(rva), Some(size)) = (
            read_u32(image, data_entry),
            read_u32(image, data_entry + 4),
        ) else {
            continue;
        };
        let Some(offset) = to_offset(rva) else {
            continue;
        };
        let Some(bytes) = image.get(offset..offset.saturating_add(size as usize)) else {
            continue;
        };
        out.push(Resource {
            type_sel: kind.clone(),
            name_sel: name.clone(),
            // The third level's id is the language.
            language: this_id.unwrap_or(0),
            data: bytes.to_vec(),
        });
    }
}

/// Read a `[len: u16][utf16 chars]` name at `off`.
fn read_name(image: &[u8], off: usize) -> Option<String> {
    let len = read_u16(image, off)? as usize;
    let bytes = image.get(off + 2..off + 2 + len * 2)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

fn read_u16(image: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(image.get(off..off + 2)?.try_into().ok()?))
}

fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(image.get(off..off + 4)?.try_into().ok()?))
}

/// Resolve a path that may differ from `path` only in case.
///
/// Resource names are matched case-insensitively on Windows, but the staged
/// files sit on whatever filesystem the analysis runs on, so fall back to a
/// scan of the parent directory.
fn resolve_ci(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if path.exists() {
        return Some(path.to_path_buf());
    }
    let dir = path.parent()?;
    let want = path.file_name()?.to_str()?;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.eq_ignore_ascii_case(want) {
                return Some(entry.path());
            }
        }
    }
    None
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../../tests/unit/winemu_pe.rs"]
mod tests;
