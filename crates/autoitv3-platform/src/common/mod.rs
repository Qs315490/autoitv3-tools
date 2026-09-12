//! Common platform — AutoIt functions that can be implemented
//! identically on every operating system.
//!
//! This layer is *not* OS-specific: it is installed on Linux **and** Windows,
//! underneath the operating system's own layer (see [`crate::CompositePlatform`]).
//! It covers the part of AutoIt's library that touches the outside world but
//! does so the same way everywhere:
//!
//! * **files** — `FileOpen`/`FileClose`/`FileRead`/`FileReadLine`/`FileWrite`/
//!   `FileWriteLine`/`FileFlush`, `FileExists`, `FileGetSize`, `FileGetTime`,
//!   `FileGetAttrib`, `FileGetLongName`/`FileGetShortName`, `FileGetPos`/
//!   `FileSetPos`/`FileSetEnd`, `FileGetEncoding`, `FileReadToArray`,
//!   `FileFindFirstFile`/`FileFindNextFile`, `FileSetTime`, `FileChangeDir`,
//!   `FileDelete`, `FileCopy`, `FileMove`, `FileSetAttrib`
//! * **directories** — `DirCreate`, `DirRemove`, `DirGetSize`, `DirCopy`,
//!   `DirMove`
//! * **ini files** — `IniRead`, `IniWrite`, `IniDelete`, `IniReadSection`,
//!   `IniReadSectionNames`, `IniRenameSection`, `IniWriteSection`
//! * **environment** — `EnvGet`, `EnvSet`, `EnvUpdate`
//! * **math** — `Round`, `Sqrt`, `Sin`, `Cos`, `Tan`, `ASin`, `ACos`, `ATan`,
//!   `Log`, `Exp`, `Floor`, `Ceiling`, `Random`, `RandomSeed`
//! * **timing** — `TimerInit`, `TimerDiff`
//! * **console** — `ConsoleWrite`, `ConsoleWriteError`, `ConsoleRead`
//! * **processes** — the `Run`/`StdoutRead` family in [`proc`]. The common
//!   layer is the *unified interface* for the family; the per-OS observation
//!   points are implemented in the system modules
//!   ([`crate::linux::proc_support`], [`crate::windows::process`]) and called
//!   from here behind three per-host hooks
//! * **network** — `Inet*`/`TCP*`/`UDP*`/`Ping` in [`net`]
//!
//! The process and network services are delegated to the [`proc`] and [`net`]
//! submodules: like the rest of this layer they are cross-platform, but unlike
//! file reads they have external effects, so they honour the
//! [`ExecutionProfile`](autoitv3_runtime::ExecutionProfile) — a deterministic
//! analysis run never starts a process or opens a socket.
//!
//! # Deliberate approximations
//!
//! A few results cannot be reproduced exactly off Windows. They are documented
//! here rather than silently guessed:
//!
//! * `FileGetTime` returns **UTC** (a local-time rendering would need a
//!   timezone database); the `YYYY/MM/DD HH:MM:SS` layout matches AutoIt.
//! * `FileGetAttrib` reports `D` for directories and `A` for regular files and
//!   adds `R` when the file is read-only; `FileSetAttrib` only understands the
//!   portable `R`/`N` flags. The Windows layer (`crate::windows`) overrides
//!   both with the real `FILE_ATTRIBUTE_*` semantics (`S`, `H`, `+`/`-`), and
//!   likewise answers `FileGetShortName` with a real 8.3 name and broadcasts
//!   `EnvUpdate`'s `WM_SETTINGCHANGE`.
//! * `Random` is deliberately **deterministic** by default (seed `0x2545F491`),
//!   so deobfuscation results are reproducible; call `RandomSeed` for AutoIt's
//!   behaviour.
//! * Text is read and written as UTF-8. The `$FO_UNICODE` family of `FileOpen`
//!   mode flags is accepted but treated as UTF-8.

pub mod net;
pub mod proc;

use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::{EffectPolicy, RandomPolicy, DEFAULT_RANDOM_SEED};
use autoitv3_runtime::value::Value;

use self::net::NetworkService;
use self::proc::ProcessService;

/// Every function this layer implements.
pub const FUNCTIONS: &[&str] = &[
    // files
    "FileOpen",
    "FileClose",
    "FileFlush",
    "FileRead",
    "FileReadLine",
    "FileWrite",
    "FileWriteLine",
    "FileExists",
    "FileGetSize",
    "FileGetTime",
    "FileGetAttrib",
    "FileGetLongName",
    "FileGetShortName",
    "FileDelete",
    "FileCopy",
    "FileMove",
    "FileSetAttrib",
    "FileChangeDir",
    "FileGetPos",
    "FileSetPos",
    "FileSetEnd",
    "FileGetEncoding",
    "FileReadToArray",
    "FileFindFirstFile",
    "FileFindNextFile",
    "FileSetTime",
    // directories
    "DirCreate",
    "DirRemove",
    "DirGetSize",
    "DirCopy",
    "DirMove",
    // ini files
    "IniRead",
    "IniWrite",
    "IniDelete",
    "IniReadSection",
    "IniReadSectionNames",
    "IniRenameSection",
    "IniWriteSection",
    // environment
    "EnvGet",
    "EnvSet",
    "EnvUpdate",
    // math
    "Round",
    "Sqrt",
    "Sin",
    "Cos",
    "Tan",
    "ASin",
    "ACos",
    "ATan",
    "Log",
    "Exp",
    "Floor",
    "Ceiling",
    "Random",
    "RandomSeed",
    "SRandom",
    // timing
    "TimerInit",
    "TimerDiff",
    // console
    "ConsoleWrite",
    "ConsoleWriteError",
    "ConsoleRead",
];

/// How a file handle was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Append,
    Overwrite,
}

struct FileEntry {
    access: Access,
    file: File,
    /// Whole-file text, loaded on demand for character/line reads.
    text: Option<String>,
    /// Character cursor used by `FileRead`.
    cursor: usize,
}

/// An open `FileFindFirstFile` search. `FileClose` releases it, like AutoIt.
struct SearchEntry {
    /// Matching entries, already in a stable order.
    matches: Vec<PathBuf>,
    /// How many have been handed out by `FileFindNextFile`.
    index: usize,
}

/// A slot in the handle table. AutoIt hands out file and search handles from
/// the same pool, so `FileClose` accepts either.
enum Handle {
    File(FileEntry),
    Search(SearchEntry),
}

/// The common platform: file/environment/math builtins plus the
/// process and network services from [`proc`] and [`net`].
pub struct CommonPlatform {
    /// Handle `n` lives at index `n - 1`; `None` is a closed slot.
    handles: Vec<Option<Handle>>,
    /// xorshift64 state. `None` until first use, so the seed can come from the
    /// execution profile (deterministic or entropy) or from `RandomSeed`.
    rng: Option<u64>,
    origin: Instant,
    proc: ProcessService,
    net: NetworkService,
}

impl Default for CommonPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl CommonPlatform {
    /// Create the common platform.
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
            rng: None,
            origin: Instant::now(),
            proc: ProcessService::new(),
            net: NetworkService::new(),
        }
    }

    // ----- helpers -----

    fn entry(&self, handle: i64) -> Option<&FileEntry> {
        if handle < 1 {
            return None;
        }
        match self.handles.get(handle as usize - 1)?.as_ref()? {
            Handle::File(e) => Some(e),
            Handle::Search(_) => None,
        }
    }

    fn entry_mut(&mut self, handle: i64) -> Option<&mut FileEntry> {
        if handle < 1 {
            return None;
        }
        match self.handles.get_mut(handle as usize - 1)?.as_mut()? {
            Handle::File(e) => Some(e),
            Handle::Search(_) => None,
        }
    }

    fn search_mut(&mut self, handle: i64) -> Option<&mut SearchEntry> {
        if handle < 1 {
            return None;
        }
        match self.handles.get_mut(handle as usize - 1)?.as_mut()? {
            Handle::Search(s) => Some(s),
            Handle::File(_) => None,
        }
    }

    /// Load the whole file into `text` if it has not been read yet.
    fn ensure_text(&mut self, handle: i64) {
        let Some(e) = self.entry_mut(handle) else { return };
        if e.text.is_some() {
            return;
        }
        let mut s = String::new();
        let _ = e.file.seek(SeekFrom::Start(0));
        let _ = e.file.read_to_string(&mut s);
        // A UTF-8 BOM is not part of the value AutoIt hands to the script.
        let s = s.strip_prefix('\u{feff}').unwrap_or(&s).to_string();
        e.text = Some(s);
    }

    /// Drop the cached text after a write so later reads see the new content.
    fn invalidate_text(&mut self, handle: i64) {
        if let Some(e) = self.entry_mut(handle) {
            e.text = None;
        }
    }

    /// Seed the generator on first use, honouring the execution profile.
    ///
    /// `RandomPolicy::Deterministic` makes a run reproducible;
    /// `RandomPolicy::Entropy` behaves like AutoIt.
    fn ensure_rng(&mut self, ctx: &dyn HostContext) {
        if self.rng.is_some() {
            return;
        }
        self.rng = Some(match ctx.profile().random {
            RandomPolicy::Deterministic(seed) => seed,
            RandomPolicy::Entropy => entropy_seed(),
        });
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng.unwrap_or(0);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = Some(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    // ----- files -----

    fn file_open(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let path = arg_str(args, 0);
        let mode = arg_int(args, 1);
        let access = match mode & 3 {
            1 => Access::Append,
            2 => Access::Overwrite,
            _ => Access::Read,
        };
        let create_path = mode & 8 != 0;

        // Opening for write creates or truncates the file: a state change.
        if access != Access::Read && !Self::writes_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(-1);
        }

        let p = PathBuf::from(&path);
        if create_path {
            if let Some(dir) = p.parent() {
                if !dir.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(dir);
                }
            }
        }
        let opened = match access {
            Access::Read => File::open(&p),
            // Write handles are opened readable too: `FileGetPos`/`FileSetPos`/
            // `FileSetEnd` need to see the bytes already on disk.
            Access::Append => OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&p),
            Access::Overwrite => OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(true)
                .open(&p),
        };
        match opened {
            Ok(file) => {
                self.handles.push(Some(Handle::File(FileEntry {
                    access,
                    file,
                    text: None,
                    cursor: 0,
                })));
                ctx.set_error(0, 0);
                Value::Int(self.handles.len() as i64)
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::Int(-1)
            }
        }
    }

    fn file_read(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let count = arg_int(args, 1);
        if self.entry(handle).is_none() {
            ctx.set_error(1, 0);
            return Value::str("");
        }
        self.ensure_text(handle);
        let Some(e) = self.entry_mut(handle) else {
            return Value::str("");
        };
        let text = e.text.as_deref().unwrap_or("");
        let chars: Vec<char> = text.chars().collect();
        let start = e.cursor.min(chars.len());
        // A non-positive count means "to the end of the file".
        let end = if count <= 0 {
            chars.len()
        } else {
            (start + count as usize).min(chars.len())
        };
        e.cursor = end;
        ctx.set_error(0, 0);
        Value::Str(chars[start..end].iter().collect())
    }

    fn file_read_line(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let line = arg_int(args, 1);
        if self.entry(handle).is_none() || line < 0 {
            ctx.set_error(1, 0);
            return Value::str("");
        }
        self.ensure_text(handle);
        let Some(e) = self.entry_mut(handle) else {
            return Value::str("");
        };
        let text = e.text.as_deref().unwrap_or("");
        let chars: Vec<char> = text.chars().collect();
        if line >= 1 {
            // An explicit line number reads that line (1-based), wherever the
            // cursor happens to be. AutoIt line endings are @CRLF, @LF or @CR.
            match chars
                .split(|&c| c == '\n')
                .map(|l: &[char]| {
                    let l = if l.last() == Some(&'\r') { &l[..l.len() - 1] } else { l };
                    l.iter().collect::<String>()
                })
                .nth(line as usize - 1)
            {
                Some(l) => {
                    ctx.set_error(0, 0);
                    Value::Str(l)
                }
                None => {
                    ctx.set_error(1, 0);
                    Value::str("")
                }
            }
        } else {
            // `FileReadLine($handle)` reads the next line from the cursor and
            // advances it; reading past the end of the file is `@error = 1`.
            let start = e.cursor.min(chars.len());
            if start >= chars.len() {
                ctx.set_error(1, 0);
                return Value::str("");
            }
            let end = chars[start..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |i| start + i);
            let l: String = chars[start..end].iter().collect();
            let l = l.strip_suffix('\r').unwrap_or(&l).to_string();
            e.cursor = (end + 1).min(chars.len());
            ctx.set_error(0, 0);
            Value::Str(l)
        }
    }

    fn file_write(&mut self, args: &[Value], line_mode: bool, ctx: &mut dyn HostContext) -> Value {
        let handle = arg_int(args, 0);
        let text = arg_str(args, 1);
        let Some(e) = self.entry_mut(handle) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        if e.access == Access::Read {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        if !Self::writes_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        // `FileWriteLine` terminates the line with @CRLF, like AutoIt.
        let payload = if line_mode {
            format!("{text}\r\n")
        } else {
            text
        };
        match e.file.write_all(payload.as_bytes()).and_then(|()| e.file.flush()) {
            Ok(()) => {
                let n = payload.len() as i64;
                self.invalidate_text(handle);
                ctx.set_error(0, 0);
                Value::Int(n)
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    // ----- directories -----

    fn dir_size(path: &Path) -> u64 {
        Self::dir_stats(path).0
    }

    /// `(bytes, files, directories)` under `path`, for `DirGetSize`.
    fn dir_stats(path: &Path) -> (u64, u64, u64) {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return (0, 0, 0);
        };
        if meta.is_file() {
            return (meta.len(), 1, 0);
        }
        let Ok(entries) = fs::read_dir(path) else {
            return (0, 0, 0);
        };
        let mut total = (0u64, 0u64, 0u64);
        for e in entries.flatten() {
            let (s, f, d) = Self::dir_stats(&e.path());
            total.0 += s;
            total.1 += f;
            total.2 += d + u64::from(e.file_type().map(|t| t.is_dir()).unwrap_or(false));
        }
        total
    }

    /// Whether the profile allows modifying state.
    fn writes_allowed(ctx: &dyn HostContext) -> bool {
        matches!(ctx.profile().effects, EffectPolicy::Allow)
    }

    // ----- dispatch -----

    fn call_inner(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Option<Value> {
        let v = match name {
            // ---------------- files ----------------
            "fileopen" => self.file_open(args, ctx),
            "fileclose" => {
                let handle = arg_int(args, 0);
                if handle >= 1 && (handle as usize) <= self.handles.len() {
                    self.handles[handle as usize - 1] = None;
                    ctx.set_error(0, 0);
                    Value::Int(1)
                } else {
                    ctx.set_error(1, 0);
                    Value::Int(0)
                }
            }
            "fileflush" => {
                let handle = arg_int(args, 0);
                let ok = self
                    .entry_mut(handle)
                    .map(|e| e.file.flush().is_ok())
                    .unwrap_or(false);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "fileread" => self.file_read(args, ctx),
            "filereadline" => self.file_read_line(args, ctx),
            "filewrite" => self.file_write(args, false, ctx),
            "filewriteline" => self.file_write(args, true, ctx),
            "fileexists" => {
                Value::Int(i64::from(Path::new(&arg_str(args, 0)).exists()))
            }
            "filegetsize" => {
                let path = arg_str(args, 0);
                let unit = arg_str(args, 1).to_ascii_uppercase();
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let scaled = match unit.as_str() {
                    "K" | "KB" => size / 1024,
                    "M" | "MB" => size / (1024 * 1024),
                    "G" | "GB" => size / (1024 * 1024 * 1024),
                    _ => size,
                };
                Value::Int(scaled as i64)
            }
            "filegettime" => {
                let path = arg_str(args, 0);
                let option = arg_int(args, 1);
                let format = arg_int(args, 2);
                let meta = fs::metadata(&path);
                let stamp = meta.as_ref().ok().and_then(|m| match option {
                    1 => m.created().ok(),
                    2 => m.accessed().ok(),
                    _ => m.modified().ok(),
                });
                match (stamp, format) {
                    (Some(t), 0) => Value::Str(format_system_time(t)),
                    (Some(t), _) => {
                        let s = format_system_time(t);
                        let parts: Vec<Value> = s
                            .split(['/', ' ', ':'])
                            .map(|p| Value::Int(p.parse().unwrap_or(0)))
                            .collect();
                        Value::array(parts)
                    }
                    (None, _) => {
                        ctx.set_error(1, 0);
                        Value::str("")
                    }
                }
            }
            "filegetattrib" => {
                let path = arg_str(args, 0);
                let Ok(meta) = fs::metadata(&path) else {
                    ctx.set_error(1, 0);
                    return Some(Value::str(""));
                };
                let mut a = String::new();
                if meta.is_dir() {
                    a.push('D');
                } else {
                    a.push('A');
                }
                if meta.permissions().readonly() {
                    a.push('R');
                }
                Value::Str(a)
            }
            "filegetlongname" => {
                let path = arg_str(args, 0);
                match fs::canonicalize(&path) {
                    Ok(p) => Value::Str(p.to_string_lossy().into_owned()),
                    Err(_) => Value::Str(path),
                }
            }
            // Short (8.3) names are a Windows concept; the long name is the
            // closest portable answer.
            "filegetshortname" => Value::Str(arg_str(args, 0)),
            "filedelete" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let ok = fs::remove_file(&path).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filecopy" | "filemove" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let src = arg_str(args, 0);
                let dst = arg_str(args, 1);
                let overwrite = arg_int(args, 2) == 1;
                if dst.is_empty() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let target = if Path::new(&dst).is_dir() {
                    let base = Path::new(&src)
                        .file_name()
                        .map(PathBuf::from)
                        .unwrap_or_default();
                    Path::new(&dst).join(base)
                } else {
                    PathBuf::from(&dst)
                };
                if !overwrite && target.exists() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                if let Some(dir) = target.parent() {
                    if !dir.as_os_str().is_empty() {
                        let _ = fs::create_dir_all(dir);
                    }
                }
                let r = if name == "filecopy" {
                    fs::copy(&src, &target).map(|_| ())
                } else {
                    fs::rename(&src, &target)
                };
                let ok = r.is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filesetattrib" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let attrib = arg_str(args, 1).to_ascii_uppercase();
                let Ok(meta) = fs::metadata(&path) else {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                };
                let mut perms = meta.permissions();
                // Only the read-only flag has a portable equivalent.
                if attrib.contains('R') {
                    perms.set_readonly(true);
                } else if attrib.contains('N') || attrib.contains('A') {
                    perms.set_readonly(false);
                }
                let ok = fs::set_permissions(&path, perms).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filechangedir" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let ok = std::env::set_current_dir(arg_str(args, 0)).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filegetpos" => {
                let handle = arg_int(args, 0);
                if self.entry(handle).is_none() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(-1));
                }
                self.ensure_text(handle);
                let pos = self.entry(handle).map(|e| e.cursor).unwrap_or(0);
                ctx.set_error(0, 0);
                Value::Int(pos as i64)
            }
            "filesetpos" => {
                let handle = arg_int(args, 0);
                let pos = arg_int(args, 1).max(0) as usize;
                if self.entry(handle).is_none() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                self.ensure_text(handle);
                let len = self
                    .entry(handle)
                    .and_then(|e| e.text.as_ref())
                    .map(|t| t.chars().count())
                    .unwrap_or(0);
                if let Some(e) = self.entry_mut(handle) {
                    e.cursor = pos.min(len);
                }
                ctx.set_error(0, 0);
                Value::Int(1)
            }
            "filesetend" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let handle = arg_int(args, 0);
                if self.entry(handle).is_none() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                self.ensure_text(handle);
                let byte_len = self
                    .entry(handle)
                    .map(|e| {
                        let text = e.text.as_deref().unwrap_or("");
                        text.chars()
                            .take(e.cursor)
                            .map(|c| c.len_utf8())
                            .sum::<usize>()
                    })
                    .unwrap_or(0);
                let ok = self
                    .entry_mut(handle)
                    .map(|e| e.file.set_len(byte_len as u64).is_ok())
                    .unwrap_or(false);
                if ok {
                    self.invalidate_text(handle);
                }
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filegetencoding" => {
                let bytes = match args.first() {
                    // A handle reads through the already-open file.
                    Some(v) if v.is_number() => {
                        let handle = v.to_int();
                        let Some(e) = self.entry(handle) else {
                            ctx.set_error(1, 0);
                            return Some(Value::Int(-1));
                        };
                        let mut buf = Vec::new();
                        let mut f = &e.file;
                        let _ = f.seek(SeekFrom::Start(0));
                        let _ = f.read_to_end(&mut buf);
                        buf
                    }
                    Some(v) => match fs::read(v.to_autoit_string()) {
                        Ok(b) => b,
                        Err(_) => {
                            ctx.set_error(1, 0);
                            return Some(Value::Int(-1));
                        }
                    },
                    None => {
                        ctx.set_error(1, 0);
                        return Some(Value::Int(-1));
                    }
                };
                ctx.set_error(0, 0);
                Value::Int(detect_encoding(&bytes))
            }
            "filereadtoarray" => {
                let path = arg_str(args, 0);
                let Ok(text) = fs::read_to_string(&path) else {
                    ctx.set_error(1, 0);
                    return Some(Value::array(vec![Value::Int(0)]));
                };
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let lines: Vec<Value> = text.lines().map(|l| Value::Str(l.to_string())).collect();
                let mut out = vec![Value::Int(lines.len() as i64)];
                out.extend(lines);
                ctx.set_error(0, 0);
                Value::array(out)
            }
            "filefindfirstfile" => {
                let pattern = arg_str(args, 0);
                let (dir, mask) = split_search_pattern(&pattern);
                let matches = search_files(&dir, &mask);
                if matches.is_empty() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(-1));
                }
                self.handles.push(Some(Handle::Search(SearchEntry {
                    matches,
                    index: 0,
                })));
                ctx.set_error(0, 0);
                Value::Int(self.handles.len() as i64)
            }
            "filefindnextfile" => {
                let handle = arg_int(args, 0);
                let flag = arg_int(args, 1);
                let (path, is_dir) = match self.search_mut(handle) {
                    Some(s) if s.index < s.matches.len() => {
                        let path = s.matches[s.index].clone();
                        s.index += 1;
                        let is_dir = path.is_dir();
                        (path, is_dir)
                    }
                    _ => {
                        ctx.set_error(1, 0);
                        return Some(Value::str(""));
                    }
                };
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // flag 0 puts "is a directory" in @extended. flag 1 asks for an
                // attribute *string*, which the interpreter's numeric @extended
                // cannot carry; documented as an approximation.
                let extended = if flag == 0 { i64::from(is_dir) } else { 0 };
                ctx.set_error(0, extended);
                Value::Str(name)
            }
            "filesettime" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let kind = arg_int(args, 2);
                let format = arg_int(args, 3);
                let Some(stamp) = parse_autoit_time(&arg_str(args, 1), format) else {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                };
                // A creation timestamp has no portable setter.
                if kind == 1 {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let Ok(file) = OpenOptions::new().write(true).open(&path) else {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                };
                let times = if kind == 2 {
                    FileTimes::new().set_accessed(stamp)
                } else {
                    FileTimes::new().set_modified(stamp)
                };
                let ok = file.set_times(times).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }

            // ---------------- ini files ----------------
            "iniread" => {
                let path = arg_str(args, 0);
                let section = arg_str(args, 1);
                let key = arg_str(args, 2);
                let default = if args.len() > 3 {
                    arg_str(args, 3)
                } else {
                    String::new()
                };
                let lines = ini_lines(&path);
                match ini_get(&lines, &section, &key) {
                    Some(v) => {
                        ctx.set_error(0, 0);
                        Value::Str(v)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Str(default)
                    }
                }
            }
            "iniwrite" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let mut lines = ini_lines(&path);
                let ok = ini_set(
                    &mut lines,
                    &arg_str(args, 1),
                    &arg_str(args, 2),
                    &arg_str(args, 3),
                ) && ini_save(&path, &lines);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "inidelete" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let section = arg_str(args, 1);
                let key = if args.len() > 2 {
                    arg_str(args, 2)
                } else {
                    String::new()
                };
                let mut lines = ini_lines(&path);
                let ok = ini_delete(&mut lines, &section, &key);
                if ok && !ini_save(&path, &lines) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "inireadsection" => {
                let lines = ini_lines(&arg_str(args, 0));
                let entries = ini_section_entries(&lines, &arg_str(args, 1));
                let mut out = vec![Value::Int(entries.len() as i64)];
                out.extend(entries.into_iter().map(Value::Str));
                ctx.set_error(if out.len() > 1 { 0 } else { 1 }, 0);
                Value::array(out)
            }
            "inireadsectionnames" => {
                let lines = ini_lines(&arg_str(args, 0));
                let names: Vec<Value> = lines
                    .iter()
                    .filter_map(|l| {
                        let t = l.trim();
                        if t.starts_with('[') && t.ends_with(']') && t.len() >= 2 {
                            Some(Value::Str(t[1..t.len() - 1].to_string()))
                        } else {
                            None
                        }
                    })
                    .collect();
                let mut out = vec![Value::Int(names.len() as i64)];
                out.extend(names);
                ctx.set_error(if out.len() > 1 { 0 } else { 1 }, 0);
                Value::array(out)
            }
            "inirenamesection" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let section = arg_str(args, 1);
                let newname = arg_str(args, 2);
                let overwrite = arg_int(args, 3) == 1;
                let mut lines = ini_lines(&path);
                let ok = ini_rename_section(&mut lines, &section, &newname, overwrite)
                    && ini_save(&path, &lines);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "iniwritesection" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let section = arg_str(args, 1);
                let start = arg_int(args, 3).max(1) as usize;
                let entries: Vec<String> = match args.get(2) {
                    Some(Value::Array(a)) => {
                        a.borrow().iter().map(|v| v.to_autoit_string()).collect()
                    }
                    Some(v) => v
                        .to_autoit_string()
                        .lines()
                        .map(|s| s.to_string())
                        .collect(),
                    None => Vec::new(),
                };
                let body: Vec<String> = entries
                    .into_iter()
                    .skip(start - 1)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut lines = ini_lines(&path);
                let ok = ini_write_section(&mut lines, &section, body) && ini_save(&path, &lines);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }

            // ---------------- directories ----------------
            "dircreate" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let ok = fs::create_dir_all(arg_str(args, 0)).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "dirremove" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let path = arg_str(args, 0);
                let recurse = arg_int(args, 1) == 1;
                let r = if recurse {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_dir(&path)
                };
                let ok = r.is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "dirgetsize" => {
                let path = arg_str(args, 0);
                // flag 1 asks for the extended array: [size, file count, dir
                // count], as AutoIt documents.
                if arg_int(args, 1) == 1 {
                    let (size, files, dirs) = Self::dir_stats(Path::new(&path));
                    Value::array(vec![
                        Value::Int(size as i64),
                        Value::Int(files as i64),
                        Value::Int(dirs as i64),
                    ])
                } else {
                    Value::Int(Self::dir_size(Path::new(&path)) as i64)
                }
            }
            "dircopy" | "dirmove" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let src = arg_str(args, 0);
                let dst = arg_str(args, 1);
                let overwrite = arg_int(args, 2) == 1;
                let from = Path::new(&src);
                let target = Path::new(&dst).join(
                    from.file_name().map(PathBuf::from).unwrap_or_default(),
                );
                if !overwrite && target.exists() {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let r = if name == "dircopy" {
                    fs::create_dir_all(&target).and_then(|()| copy_dir(from, &target))
                } else if target.exists() {
                    Err(std::io::Error::other("target exists"))
                } else {
                    fs::rename(from, &target)
                };
                let ok = r.is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }

            // ---------------- environment ----------------
            "envget" => Value::Str(std::env::var(arg_str(args, 0)).unwrap_or_default()),
            "envset" => {
                if !Self::writes_allowed(ctx) {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                }
                let key = arg_str(args, 0);
                if args.len() > 1 {
                    std::env::set_var(key, arg_str(args, 1));
                } else {
                    std::env::remove_var(key);
                }
                Value::Int(1)
            }
            // On Windows the system layer answers for real (a
            // `WM_SETTINGCHANGE` broadcast); nothing to refresh elsewhere.
            "envupdate" => Value::Int(1),

            // ---------------- math ----------------
            "round" => {
                let x = arg_f64(args, 0);
                let digits = arg_int(args, 1).clamp(0, 15) as i32;
                let factor = 10f64.powi(digits);
                // AutoIt rounds halves away from zero, as `f64::round` does.
                Value::Float((x * factor).round() / factor)
            }
            "sqrt" => Value::Float(arg_f64(args, 0).sqrt()),
            // Trigonometry is in radians, as documented by AutoIt.
            "sin" => Value::Float(arg_f64(args, 0).sin()),
            "cos" => Value::Float(arg_f64(args, 0).cos()),
            "tan" => Value::Float(arg_f64(args, 0).tan()),
            "asin" => Value::Float(arg_f64(args, 0).asin()),
            "acos" => Value::Float(arg_f64(args, 0).acos()),
            "atan" => Value::Float(arg_f64(args, 0).atan()),
            "log" => Value::Float(arg_f64(args, 0).ln()),
            "exp" => Value::Float(arg_f64(args, 0).exp()),
            "floor" => Value::Float(arg_f64(args, 0).floor()),
            "ceiling" => Value::Float(arg_f64(args, 0).ceil()),
            // `SRandom` is the legacy spelling of `RandomSeed`.
            "randomseed" | "srandom" => {
                let seed = arg_int(args, 0) as u64;
                self.rng = Some(if seed == 0 { DEFAULT_RANDOM_SEED } else { seed });
                Value::Int(1)
            }
            "random" => {
                self.ensure_rng(ctx);
                if args.is_empty() {
                    let r = self.next_u64() >> 11;
                    Value::Float(r as f64 / (1u64 << 53) as f64)
                } else {
                    let lo = arg_int(args, 0);
                    let hi = arg_int(args, 1).max(lo);
                    let as_float = arg_int(args, 2) == 1;
                    let span = (hi - lo + 1) as u64;
                    let pick = lo + (self.next_u64() % span.max(1)) as i64;
                    if as_float {
                        Value::Float(pick as f64)
                    } else {
                        Value::Int(pick)
                    }
                }
            }

            // ---------------- timing ----------------
            "timerinit" => {
                // An opaque handle: microseconds since this platform was made.
                Value::Int(self.origin.elapsed().as_micros() as i64)
            }
            "timerdiff" => {
                let started = arg_int(args, 0);
                let now = self.origin.elapsed().as_micros() as i64;
                Value::Float((now - started) as f64 / 1000.0)
            }

            // ---------------- console ----------------
            "consolewrite" => {
                let text = arg_str(args, 0);
                let mut out = std::io::stdout();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
                Value::Int(text.chars().count() as i64)
            }
            "consolewriteerror" => {
                let text = arg_str(args, 0);
                let mut err = std::io::stderr();
                let _ = err.write_all(text.as_bytes());
                let _ = err.flush();
                Value::Int(text.chars().count() as i64)
            }
            "consoleread" => {
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                Value::Str(line.trim_end_matches(['\r', '\n']).to_string())
            }

            // Not a file/environment/math builtin: hand off to the process and
            // network services before giving up, so this is one common layer.
            _ => {
                if let Some(v) = self.proc.call(name, args, ctx) {
                    return Some(v);
                }
                return self.net.call(name, args, ctx);
            }
        };
        Some(v)
    }
}

impl Platform for CommonPlatform {
    fn name(&self) -> &'static str {
        "common"
    }

    /// Macros whose meaning is the same on every operating system.
    ///
    /// `@ScriptDir`/`@ScriptName` describe the running script, which the
    /// interpreter is not told; the working directory is the closest portable
    /// answer and is documented as such.
    fn macro_value(&self, name: &str) -> Option<Value> {
        let dir_with_sep = |p: std::path::PathBuf| {
            let mut s = p.to_string_lossy().into_owned();
            if !s.ends_with(std::path::MAIN_SEPARATOR) {
                s.push(std::path::MAIN_SEPARATOR);
            }
            Value::Str(s)
        };
        let home = || std::env::var("HOME").ok().filter(|h| !h.is_empty());
        let value = match name {
            "tempdir" => dir_with_sep(std::env::temp_dir()),
            "workingdir" | "scriptdir" => match std::env::current_dir() {
                Ok(d) => dir_with_sep(d),
                Err(_) => Value::Str(String::new()),
            },
            "autoitpid" => Value::Int(std::process::id() as i64),
            "autoitexe" => std::env::current_exe()
                .map(|p| Value::Str(p.to_string_lossy().into_owned()))
                .unwrap_or(Value::Str(String::new())),
            "username" => Value::Str(
                std::env::var("USER")
                    .or_else(|_| std::env::var("LOGNAME"))
                    .unwrap_or_default(),
            ),
            "computerName" | "computername" => std::env::var("HOSTNAME")
                .ok()
                .map(Value::Str)
                .unwrap_or(Value::Str(String::new())),
            "homepath" | "userprofiledir" => Value::Str(home().unwrap_or_default()),
            // XDG base directories, falling back to the conventional paths.
            "appdatadir" => Value::Str(
                std::env::var("XDG_CONFIG_HOME")
                    .ok()
                    .or_else(|| home().map(|h| format!("{h}/.config")))
                    .unwrap_or_default(),
            ),
            "localappdatadir" => Value::Str(
                std::env::var("XDG_DATA_HOME")
                    .ok()
                    .or_else(|| home().map(|h| format!("{h}/.local/share")))
                    .unwrap_or_default(),
            ),
            "desktopdir" => Value::Str(
                home()
                    .map(|h| format!("{h}/Desktop"))
                    .unwrap_or_default(),
            ),
            "mydocumentsdir" => Value::Str(
                home()
                    .map(|h| format!("{h}/Documents"))
                    .unwrap_or_default(),
            ),
            _ => return None,
        };
        Some(value)
    }

    fn provides(&self, name: &str) -> bool {
        FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
            || ProcessService::provides(name)
            || NetworkService::provides(name)
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let key = name.to_ascii_lowercase();
        Ok(self.call_inner(&key, &args, ctx))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

fn arg_f64(args: &[Value], i: usize) -> f64 {
    args.get(i).map(|v| v.to_f64()).unwrap_or(0.0)
}

/// A best-effort entropy seed (not cryptographic).
///
/// Mixes the clock with the process id and an address from the stack, which is
/// enough to make a faithful run differ from the next one.
fn entropy_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    // splitmix64 finaliser
    let mut z = nanos ^ pid.rotate_left(17) ^ addr.rotate_left(31);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Recursively copy `from` into the existing directory `to`.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Render a `SystemTime` as AutoIt's `YYYY/MM/DD HH:MM:SS` (UTC).
fn format_system_time(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}/{m:02}/{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day).
///
/// Howard Hinnant's `civil_from_days`, which is exact for all dates we care
/// about and needs no timezone or calendar crate.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Detect a text encoding the way `FileGetEncoding` reports it: a BOM first,
/// then a UTF-8 validity probe.
fn detect_encoding(bytes: &[u8]) -> i64 {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return 32; // $FO_UTF16_LE
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return 64; // $FO_UTF16_BE
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return 128; // $FO_UTF8 (with BOM)
    }
    if bytes.is_empty() {
        return 0;
    }
    if std::str::from_utf8(bytes).is_ok() {
        if bytes.iter().any(|b| *b > 0x7f) {
            256 // valid UTF-8 without a BOM
        } else {
            0 // plain ASCII
        }
    } else {
        512 // $FO_ANSI
    }
}

/// Split a `FileFindFirstFile` pattern into `(directory, file mask)`.
fn split_search_pattern(pattern: &str) -> (PathBuf, String) {
    let p = Path::new(pattern);
    let dir = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let mask = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "*".to_string());
    (dir, mask)
}

/// Wildcard match with `*` and `?`, case-insensitive like the Windows API.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let n: Vec<char> = name.to_ascii_lowercase().chars().collect();
    // Greedy two-pointer glob match with backtracking on the last `*`.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Entries of `dir` matching `mask`, sorted so a run is reproducible.
fn search_files(dir: &Path, mask: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| wildcard_match(mask, n))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// `"YYYY/MM/DD HH:MM:SS"` (or the digit-only spelling) -> `SystemTime`.
/// An empty string means "now", matching AutoIt.
fn parse_autoit_time(s: &str, _format: i64) -> Option<SystemTime> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Some(SystemTime::now());
    }
    if digits.len() < 14 {
        return None;
    }
    let y: i64 = digits[0..4].parse().ok()?;
    let mo: u32 = digits[4..6].parse().ok()?;
    let d: u32 = digits[6..8].parse().ok()?;
    let h: u64 = digits[8..10].parse().ok()?;
    let mi: u64 = digits[10..12].parse().ok()?;
    let sec: u64 = digits[12..14].parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let secs = days_from_civil(y, mo, d) * 86_400 + (h * 3600 + mi * 60 + sec) as i64;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Days since 1970-01-01 for a civil date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// INI helpers
// ---------------------------------------------------------------------------

fn ini_lines(path: &str) -> Vec<String> {
    fs::read_to_string(path)
        .map(|t| t.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

fn ini_save(path: &str, lines: &[String]) -> bool {
    let mut out = String::new();
    for l in lines {
        out.push_str(l);
        out.push_str("\r\n");
    }
    fs::write(path, out).is_ok()
}

/// `(header index, end index)` for a section, or `None` when it is absent.
fn ini_section_bounds(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let want = section.trim().trim_start_matches('[').trim_end_matches(']');
    let mut start: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            let name = t.trim_start_matches('[').trim_end_matches(']');
            match start {
                None if name.eq_ignore_ascii_case(want) => start = Some(i),
                Some(s) => return Some((s, i)),
                None => {}
            }
        }
    }
    start.map(|s| (s, lines.len()))
}

fn ini_get(lines: &[String], section: &str, key: &str) -> Option<String> {
    let (s, e) = ini_section_bounds(lines, section)?;
    let want = key.trim();
    for l in &lines[s + 1..e] {
        let t = l.trim();
        if t.is_empty() || t.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().eq_ignore_ascii_case(want) {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn ini_set(lines: &mut Vec<String>, section: &str, key: &str, value: &str) -> bool {
    let sec = section.trim().trim_start_matches('[').trim_end_matches(']');
    let key = key.trim();
    if sec.is_empty() || key.is_empty() {
        return false;
    }
    match ini_section_bounds(lines, sec) {
        Some((s, e)) => {
            for i in s + 1..e {
                let t = lines[i].trim();
                if t.is_empty() || t.starts_with(';') {
                    continue;
                }
                if let Some((k, _)) = t.split_once('=') {
                    if k.trim().eq_ignore_ascii_case(key) {
                        lines[i] = format!("{key}={value}");
                        return true;
                    }
                }
            }
            lines.insert(e, format!("{key}={value}"));
            true
        }
        None => {
            if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            lines.push(format!("[{sec}]"));
            lines.push(format!("{key}={value}"));
            true
        }
    }
}

fn ini_delete(lines: &mut Vec<String>, section: &str, key: &str) -> bool {
    let Some((s, e)) = ini_section_bounds(lines, section) else {
        return false;
    };
    if key.trim().is_empty() {
        lines.drain(s..e);
        return true;
    }
    let want = key.trim();
    for i in s + 1..e {
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with(';') {
            continue;
        }
        if let Some((k, _)) = t.split_once('=') {
            if k.trim().eq_ignore_ascii_case(want) {
                lines.remove(i);
                return true;
            }
        }
    }
    false
}

/// The non-comment `key=value` lines of a section.
fn ini_section_entries(lines: &[String], section: &str) -> Vec<String> {
    let Some((s, e)) = ini_section_bounds(lines, section) else {
        return Vec::new();
    };
    lines[s + 1..e]
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|t| !t.is_empty() && !t.starts_with(';') && t.contains('='))
        .collect()
}

fn ini_rename_section(
    lines: &mut Vec<String>,
    section: &str,
    newname: &str,
    overwrite: bool,
) -> bool {
    let newname = newname.trim().trim_start_matches('[').trim_end_matches(']');
    if newname.is_empty() || ini_section_bounds(lines, section).is_none() {
        return false;
    }
    if let Some((ns, ne)) = ini_section_bounds(lines, newname) {
        if !overwrite {
            return false;
        }
        lines.drain(ns..ne);
    }
    let Some((s, _)) = ini_section_bounds(lines, section) else {
        return false;
    };
    lines[s] = format!("[{newname}]");
    true
}

fn ini_write_section(lines: &mut Vec<String>, section: &str, body: Vec<String>) -> bool {
    let sec = section.trim().trim_start_matches('[').trim_end_matches(']');
    if sec.is_empty() {
        return false;
    }
    match ini_section_bounds(lines, sec) {
        Some((s, e)) => {
            lines.splice(s + 1..e, body);
        }
        None => {
            if lines.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            lines.push(format!("[{sec}]"));
            lines.extend(body);
        }
    }
    true
}

/// Read a file as lines; used by tests and by `linux/mod.rs` style helpers.
#[allow(dead_code)]
pub(crate) fn read_lines(path: &Path) -> Vec<String> {
    let Ok(f) = File::open(path) else { return Vec::new() };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .collect()
}