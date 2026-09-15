//! Mapping the emulated `C:` drive onto a host directory.
//!
//! A Windows-targeted script does not merely *print* paths — it takes them
//! apart. `@ScriptDir & "\data.dat"` is only the simplest case; the common one
//! is a hand-written normaliser that splits on `\`, looks for a drive letter or
//! a `\\server` prefix, and collapses `.`/`..`. Handing such code a POSIX path
//! makes it produce nonsense (`/home/me/x` has no drive, so the first component
//! is taken as one), which is why the emulation hands out Windows-shaped paths
//! and translates them back at the boundary where the host filesystem is
//! actually touched.
//!
//! [`PathMap`] is that translation. It is deliberately a plain value, so a
//! caller chooses the root: the emulation defaults to `C:\` = the host root
//! (`C:\home\me\a.dat` is `/home/me/a.dat`), and `AU3_WIN_DRIVE_MAP=0` or
//! [`crate::WindowsEmulation::without_path_map`] turns it off entirely, which
//! leaves every path a host path.

use std::path::{Path, PathBuf};

/// One emulated drive, and the host directory it stands for.
///
/// Both directions are supported: [`to_host`](Self::to_host) turns an emulated
/// path into something the host can open, and
/// [`to_windows`](Self::to_windows) renders a host path the way the script
/// expects to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMap {
    /// Emulated drive, without a separator, e.g. `C:`.
    drive: String,
    /// Host directory the drive stands for, e.g. `/` or `C:\`.
    root: PathBuf,
}

impl PathMap {
    /// Map `drive` (with or without its colon, in either case) onto `root`.
    pub fn new(drive: impl AsRef<str>, root: impl Into<PathBuf>) -> Self {
        let drive = drive.as_ref().trim().trim_end_matches(['\\', '/']);
        Self {
            drive: drive.to_string(),
            root: root.into(),
        }
    }

    /// The default: the host root *is* `C:\`.
    ///
    /// On a POSIX host that makes `C:\home\me\a.dat` and `/home/me/a.dat` the
    /// same file; on Windows it is the identity map, which is exactly what the
    /// fallback emulation layer there wants.
    pub fn host_root() -> Self {
        #[cfg(windows)]
        {
            Self::new("C:", PathBuf::from(r"C:\"))
        }
        #[cfg(not(windows))]
        {
            Self::new("C:", PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
        }
    }

    /// The emulated drive (`C:`).
    pub fn drive(&self) -> &str {
        &self.drive
    }

    /// The host directory the drive stands for.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `path` is on the mapped drive, i.e. whether
    /// [`to_windows`](Self::to_windows) can render it as `C:\...`.
    pub fn covers(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    /// The part of `path` after the drive, or `None` when `path` is not on it.
    ///
    /// A bare `C:` counts (it is the drive root); `C:silly` does not.
    fn strip_drive<'a>(&self, path: &'a str) -> Option<&'a str> {
        let (head, rest) = path.split_at_checked(2)?;
        if !head.eq_ignore_ascii_case(&self.drive) {
            return None;
        }
        if rest.is_empty() || rest.starts_with(['\\', '/']) {
            Some(rest)
        } else {
            None
        }
    }

    /// `C:\dir\file` → `<root>/dir/file`; `None` when `path` is not on the
    /// mapped drive.
    pub fn to_host(&self, path: &str) -> Option<PathBuf> {
        let rest = self.strip_drive(path)?;
        let mut out = self.root.clone();
        for part in rest.split(['\\', '/']) {
            match part {
                "" | "." => {}
                other => out.push(other),
            }
        }
        Some(out)
    }

    /// `<root>/dir/file` → `C:\dir\file`.
    ///
    /// A path outside the root is returned the way the host spells it: there is
    /// nothing to map it against, and inventing a drive for it would be worse
    /// than the host path the caller already has.
    pub fn to_windows(&self, path: &Path) -> String {
        let Ok(rest) = path.strip_prefix(&self.root) else {
            return path.to_string_lossy().into_owned();
        };
        let mut out = self.drive.clone();
        let mut any = false;
        for part in rest.to_string_lossy().split(std::path::MAIN_SEPARATOR) {
            if part.is_empty() {
                continue;
            }
            out.push('\\');
            out.push_str(part);
            any = true;
        }
        if !any {
            // The root itself is `C:\`, not `C:`.
            out.push('\\');
        }
        out
    }

    /// Rewrite one argument: an emulated path becomes a host path, and any
    /// other Windows-style path has its separators normalised for the host.
    ///
    /// The second half is what makes a *relative* `sub\file.txt` work on a
    /// POSIX host; a string that is not a path at all (`"C:not a drive"`) is
    /// left alone.
    pub fn rewrite(&self, path: &str) -> String {
        if let Some(host) = self.to_host(path) {
            return host.to_string_lossy().into_owned();
        }
        if std::path::MAIN_SEPARATOR == '\\' || !path.contains('\\') {
            return path.to_string();
        }
        path.replace('\\', std::path::MAIN_SEPARATOR_STR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_a_drive_path_onto_the_root() {
        let map = PathMap::new("C:", "/");
        assert_eq!(
            map.to_host(r"C:\home\me\a.dat").unwrap(),
            PathBuf::from("/home/me/a.dat")
        );
        assert_eq!(map.to_host(r"c:/home/me").unwrap(), PathBuf::from("/home/me"));
        assert_eq!(map.to_host(r"C:\").unwrap(), PathBuf::from("/"));
        assert_eq!(map.to_host("C:").unwrap(), PathBuf::from("/"));
        assert_eq!(map.to_host(r"C:\a\..\b").unwrap(), PathBuf::from("/a/../b"));
    }

    #[test]
    fn leaves_paths_that_are_not_on_the_drive_alone() {
        let map = PathMap::new("C:", "/");
        assert_eq!(map.to_host(r"D:\other"), None);
        assert_eq!(map.to_host("/home/me"), None);
        assert_eq!(map.to_host(r"C:silly"), None);
        assert_eq!(map.to_host(""), None);
        assert_eq!(map.to_host("C"), None);
    }

    #[test]
    fn renders_a_host_path_the_way_the_script_expects() {
        let map = PathMap::new("C:", "/");
        assert_eq!(map.to_windows(Path::new("/home/me/a.dat")), r"C:\home\me\a.dat");
        assert_eq!(map.to_windows(Path::new("/")), r"C:\");
        // Outside the mapped root: the host spelling is the honest answer.
        let narrow = PathMap::new("C:", "/srv/run");
        assert_eq!(narrow.to_windows(Path::new("/etc/hosts")), "/etc/hosts");
    }

    #[test]
    fn rewrite_handles_drive_and_relative_paths() {
        let map = PathMap::new("C:", "/");
        assert_eq!(map.rewrite(r"C:\tmp\a.txt"), "/tmp/a.txt");
        assert_eq!(map.rewrite("plain text"), "plain text");
        #[cfg(not(windows))]
        assert_eq!(map.rewrite(r"sub\file.txt"), "sub/file.txt");
    }
}
