//! The file-backed half of a build's resource chain.
//!
//! A compiled script reads its payload out of **its own image**:
//! `GetModuleHandleW(NULL)` → `FindResourceW` → `SizeofResource` →
//! `LoadResource` → `LockResource` → `RtlMoveMemory`. An analysis usually has
//! the payload *files* and not the `.exe` they were packed into, though:
//! `#AutoIt3Wrapper_Res_File_Add` names each file the wrapper embedded, and
//! unpacking a build leaves the same bytes next to the script under
//! `__NAME`/`NAME`/`__Res64/NAME`/`__ResImage/_NAME`.
//!
//! [`ResourceFiles`] is that half of the chain, shared by the two hosts that
//! answer it — the emulation layer off Windows, and `file_layer` in front of the
//! native one on it (where the real `FindResourceW` only knows about mapped
//! images).

use std::path::{Path, PathBuf};

use crate::winfmt::{pe, Selector};

#[cfg(any(windows, test))]
pub(crate) mod file_layer;

/// The files a resource lookup can fall back to when the image has nothing.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResourceFiles {
    /// The directories searched, in order: the script's own, then the working
    /// directory.
    dirs: Vec<PathBuf>,
    /// The script's own names, `(resource name, file as written)`.
    aliases: Vec<(String, String)>,
}

impl ResourceFiles {
    /// Replace the search directories.
    pub(crate) fn with_dirs(&mut self, dirs: impl IntoIterator<Item = impl Into<PathBuf>>) {
        self.dirs = dirs.into_iter().map(Into::into).collect();
    }

    /// Replace the script's own `_Res_File_Add` table.
    pub(crate) fn with_aliases(&mut self, aliases: impl IntoIterator<Item = (String, String)>) {
        self.aliases = aliases.into_iter().collect();
    }

    /// The directories searched, in order.
    pub(crate) fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Whether there is anything here to answer a lookup from.
    ///
    /// Costs a directory scan, so a caller that needs it more than once should
    /// keep the answer. The Windows layer is the only caller in production
    /// builds; off Windows this is the tests'.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn is_empty(&self) -> bool {
        self.aliases.is_empty() && !crate::winemu::has_staged_resources(&self.dirs)
    }

    /// The bytes for a resource name: the script's own table first, then the
    /// files staged next to it.
    pub(crate) fn find(&self, name: &Selector) -> Option<Vec<u8>> {
        if let Some(bytes) = self.find_alias(name) {
            return Some(bytes);
        }
        pe::PeImage::find_resource_file(&self.dirs, name)
    }

    /// The file the script's own `_Res_File_Add` table maps this name to.
    ///
    /// Only string selectors can match: the table is keyed by the name the
    /// build script wrote. The file is tried as written, with `\` read as a
    /// separator, and relative to the search directories — a build machine's
    /// path does not exist here, but the file it named usually sits next to the
    /// script.
    fn find_alias(&self, name: &Selector) -> Option<Vec<u8>> {
        let wanted = name.name.as_deref()?;
        for (resource, file) in &self.aliases {
            if !resource.eq_ignore_ascii_case(wanted) {
                continue;
            }
            for candidate in [file.clone(), file.replace('\\', "/")] {
                if let Some(bytes) = self.read_file(&candidate) {
                    return Some(bytes);
                }
            }
        }
        None
    }

    /// Read one candidate path, relative to the search directories when it is
    /// not absolute. Case-insensitive, like the lookup it serves.
    fn read_file(&self, file: &str) -> Option<Vec<u8>> {
        if let Some(path) = pe::resolve_ci(Path::new(file)) {
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(bytes);
            }
        }
        for dir in &self.dirs {
            if let Some(path) = pe::resolve_ci(&dir.join(file)) {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "../../tests/unit/resources.rs"]
mod tests;
