//! Portable platform — AutoIt functions that can be implemented identically on
//! every operating system.
//!
//! This layer is *not* OS-specific: it is installed on Linux **and** Windows,
//! underneath the operating system's own layer (see [`crate::CompositePlatform`]).
//! It covers the part of AutoIt's library that touches the outside world but
//! does so the same way everywhere:
//!
//! * **files** — `FileOpen`/`FileClose`/`FileRead`/`FileReadLine`/`FileWrite`/
//!   `FileWriteLine`/`FileFlush`, `FileExists`, `FileGetSize`, `FileGetTime`,
//!   `FileGetAttrib`, `FileGetLongName`/`FileGetShortName`, `FileDelete`,
//!   `FileCopy`, `FileMove`, `FileSetAttrib`
//! * **directories** — `DirCreate`, `DirRemove`, `DirGetSize`, `DirCopy`,
//!   `DirMove`
//! * **environment** — `EnvGet`, `EnvSet`, `EnvUpdate`
//! * **math** — `Round`, `Sqrt`, `Sin`, `Cos`, `Tan`, `ASin`, `ACos`, `ATan`,
//!   `Log`, `Exp`, `Floor`, `Ceiling`, `Random`, `RandomSeed`
//! * **timing** — `TimerInit`, `TimerDiff`
//! * **console** — `ConsoleWrite`, `ConsoleWriteError`, `ConsoleRead`
//!
//! # Deliberate approximations
//!
//! A few results cannot be reproduced exactly off Windows. They are documented
//! here rather than silently guessed:
//!
//! * `FileGetTime` returns **UTC** (a local-time rendering would need a
//!   timezone database); the `YYYY/MM/DD HH:MM:SS` layout matches AutoIt.
//! * `FileGetAttrib` reports `D` for directories and `A` for regular files and
//!   adds `R` when the file is read-only. The Windows-only `S` (system) and
//!   `H` (hidden) attributes have no portable equivalent and are never set.
//! * `Random` is deliberately **deterministic** by default (seed `0x2545F491`),
//!   so deobfuscation results are reproducible; call `RandomSeed` for AutoIt's
//!   behaviour.
//! * Text is read and written as UTF-8. The `$FO_UNICODE` family of `FileOpen`
//!   mode flags is accepted but treated as UTF-8.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::{EffectPolicy, RandomPolicy, DEFAULT_RANDOM_SEED};
use autoitv3_runtime::value::Value;

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
    // directories
    "DirCreate",
    "DirRemove",
    "DirGetSize",
    "DirCopy",
    "DirMove",
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

/// The portable platform.
pub struct PortablePlatform {
    files: Vec<Option<FileEntry>>,
    /// xorshift64 state. `None` until first use, so the seed can come from the
    /// execution profile (deterministic or entropy) or from `RandomSeed`.
    rng: Option<u64>,
    origin: Instant,
}

impl Default for PortablePlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl PortablePlatform {
    /// Create the portable platform.
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            rng: None,
            origin: Instant::now(),
        }
    }

    // ----- helpers -----

    fn entry(&self, handle: i64) -> Option<&FileEntry> {
        if handle < 1 {
            return None;
        }
        self.files.get(handle as usize - 1)?.as_ref()
    }

    fn entry_mut(&mut self, handle: i64) -> Option<&mut FileEntry> {
        if handle < 1 {
            return None;
        }
        self.files.get_mut(handle as usize - 1)?.as_mut()
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
            Access::Append => OpenOptions::new().create(true).append(true).open(&p),
            Access::Overwrite => OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&p),
        };
        match opened {
            Ok(file) => {
                self.files.push(Some(FileEntry {
                    access,
                    file,
                    text: None,
                    cursor: 0,
                }));
                ctx.set_error(0, 0);
                Value::Int(self.files.len() as i64)
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
        if self.entry(handle).is_none() || line < 1 {
            ctx.set_error(1, 0);
            return Value::str("");
        }
        self.ensure_text(handle);
        let Some(e) = self.entry_mut(handle) else {
            return Value::str("");
        };
        let text = e.text.as_deref().unwrap_or("");
        // AutoIt line endings are @CRLF, @LF or @CR.
        let lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        match lines.get(line as usize - 1) {
            Some(l) => {
                ctx.set_error(0, 0);
                Value::Str((*l).to_string())
            }
            None => {
                ctx.set_error(1, 0);
                Value::str("")
            }
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
        let Ok(meta) = fs::metadata(path) else { return 0 };
        if meta.is_file() {
            return meta.len();
        }
        let Ok(entries) = fs::read_dir(path) else { return 0 };
        let mut total = 0u64;
        for e in entries.flatten() {
            total += Self::dir_size(&e.path());
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
                if handle >= 1 && (handle as usize) <= self.files.len() {
                    self.files[handle as usize - 1] = None;
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
                Value::Int(Self::dir_size(Path::new(&path)) as i64)
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
            // There is no environment block to broadcast to on Unix.
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
            "randomseed" => {
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

            _ => return None,
        };
        Some(v)
    }
}

impl Platform for PortablePlatform {
    fn name(&self) -> &'static str {
        "portable"
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

/// Read a file as lines; used by tests and by `linux.rs` style helpers.
#[allow(dead_code)]
pub(crate) fn read_lines(path: &Path) -> Vec<String> {
    let Ok(f) = File::open(path) else { return Vec::new() };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .collect()
}