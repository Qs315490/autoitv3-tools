//! Windows emulation — a Windows-flavoured [`Platform`] for non-Windows hosts.
//!
//! AutoIt is a Windows automation language, and a large part of its function
//! library is a thin wrapper over Win32. Off Windows the honest default is "not
//! provided" (see [`crate::linux`]), which is exactly right when you want to
//! *know* the boundary — but wrong when the goal is to **run a Windows-targeted
//! script far enough to learn what it computes**. A script's string table, for
//! instance, is built by asking the OS for its version:
//!
//! ```autoit
//! Local $t = DllStructCreate("struct;dword OSVersionInfoSize;dword MajorVersion;" & _
//!     "dword MinorVersion;dword BuildNumber;dword PlatformId;" & _
//!     "wchar CSDVersion[128];endstruct")
//! DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
//! DllCall("kernel32.dll", "int", "GetVersionExW", "ptr", $t)
//! ```
//!
//! This layer answers calls like those from an **emulated** machine instead of
//! failing them, so the script keeps going.
//!
//! # What is emulated
//!
//! | area | behaviour |
//! |---|---|
//! | OS identity | [`WindowsVersion`] drives `@OSVersion`, `@OSType`, `@OSBuild`, `@OSServicePack`, `@OSArch`/`@ProcessorArch`, … |
//! | paths | `@WindowsDir`, `@SystemDir`, `@ProgramFilesDir`, `@TempDir`, `@AppDataDir`, … with the conventional `C:` layout ([`WindowsPaths`]) |
//! | native structs | `DllStructCreate`/`GetData`/`SetData`/`GetSize`/`GetPtr`, backed by a byte buffer ([`DllStruct`]) |
//! | native calls | `DllCall` for the version queries (`GetVersionExW`/`A`, `RtlGetVersion`, `GetVersion`) and `GetSystemInfo` |
//! | registry | `RegRead`/`RegWrite`/`RegDelete`/`RegEnumKey`/`RegEnumVal` through a pluggable [`RegistryStore`]: by default a [`FileRegistry`] on `.au3_registry` (seeded per version, persisted on write), with [`MemoryRegistry`] available via `with_memory_registry()` |
//! | clipboard | `ClipGet`/`ClipPut` through a file in the working directory |
//! | drives | `DriveGet*`, `DriveSpace*` against a configurable [`DriveSpec`] list |
//!
//! # Choosing the emulated system
//!
//! The version defaults to **Windows 10** and can be selected three ways:
//!
//! ```no_run
//! use autoitv3_platform::winemu::{WindowsEmulation, WindowsVersion};
//!
//! // 1. In code:
//! let emu = WindowsEmulation::new().with_version(WindowsVersion::Win11);
//!
//! // 2. Through the environment (`AU3_WIN_VERSION=win11`, `AU3_WIN_ARCH=x64`,
//! //    `AU3_WIN_REGISTRY=/tmp/my.au3reg`):
//! let emu = WindowsEmulation::from_env();
//!
//! // 3. On the CLI (`au3 evaluate --win-version win11 ...`), which builds the
//! //    platform stack for you.
//! ```
//!
//! # What is *not* emulated
//!
//! Real Win32 behaviour. There is no PE loader, no COM, no GUI, no real
//! `DllCall`: a struct is a `Vec<u8>` this layer owns, and an unimplemented
//! `DllCall` sets `@error = 1` and returns `0` rather than inventing a result.
//! That keeps a script's own error handling in charge — and when you would
//! rather stop at the boundary, install [`crate::host_platform`] without this
//! layer (set `AU3_WIN_EMU=0`, or use `--no-win-emu`).

mod dllstruct;
mod paths;
mod registry;
mod version;

pub use dllstruct::{DllStruct, FieldSelector};
pub use paths::WindowsPaths;
pub use registry::{FileRegistry, MemoryRegistry, RegistryData, RegistryStore};
pub use version::{WindowsArch, WindowsVersion};

use std::path::PathBuf;
use std::time::Instant;

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::EffectPolicy;
use autoitv3_runtime::value::Value;

/// Environment variable selecting the emulated version (`win10`, `win11`, ...).
pub const VERSION_ENV: &str = "AU3_WIN_VERSION";
/// Environment variable selecting the emulated architecture (`x64`, `x86`).
pub const ARCH_ENV: &str = "AU3_WIN_ARCH";
/// Set this to `0`/`false`/`off` to leave the layer out of the stack.
pub const ENABLE_ENV: &str = "AU3_WIN_EMU";
/// Environment variable naming the file the emulated registry lives in.
pub const REGISTRY_ENV: &str = "AU3_WIN_REGISTRY";
/// The version used when nothing selects one.
pub const DEFAULT_VERSION: WindowsVersion = WindowsVersion::Win10;
/// The clipboard file, relative to the working directory.
pub const DEFAULT_CLIPBOARD_FILE: &str = ".au3_clipboard";
/// The registry file, relative to the working directory.
pub const DEFAULT_REGISTRY_FILE: &str = ".au3_registry";

/// Every function this layer implements.
pub const FUNCTIONS: &[&str] = &[
    // native structs
    "DllStructCreate",
    "DllStructGetData",
    "DllStructSetData",
    "DllStructGetSize",
    "DllStructGetPtr",
    "IsDllStruct",
    // native calls
    "DllCall",
    // registry
    "RegRead",
    "RegWrite",
    "RegDelete",
    "RegEnumKey",
    "RegEnumVal",
    // clipboard
    "ClipGet",
    "ClipPut",
    // drives
    "DriveGetDrive",
    "DriveGetType",
    "DriveGetFilesystem",
    "DriveGetLabel",
    "DriveGetSerial",
    "DriveSpaceTotal",
    "DriveSpaceFree",
    "DriveStatus",
];

/// One emulated drive.
///
/// A Windows script that walks `DriveGetDrive("FIXED")` needs at least one
/// drive to find; the default is a fixed `C:` with a nominal size. Everything
/// is explicit so an embedder can mirror the machine a sample targets.
#[derive(Debug, Clone)]
pub struct DriveSpec {
    /// Drive letter, e.g. `'C'`.
    pub letter: char,
    /// What `DriveGetType` reports: `FIXED`, `REMOVABLE`, `CDROM`,
    /// `NETWORK`, `RAMDISK` or `UNKNOWN`.
    pub kind: String,
    /// What `DriveGetFilesystem` reports, e.g. `NTFS`.
    pub filesystem: String,
    /// Volume label, or empty.
    pub label: String,
    /// Volume serial number.
    pub serial: u32,
    /// Total size in megabytes, as `DriveSpaceTotal` reports it.
    pub total_mb: u64,
    /// Free space in megabytes, as `DriveSpaceFree` reports it.
    pub free_mb: u64,
    /// Whether `DriveStatus` reports `READY`.
    pub ready: bool,
}

impl Default for DriveSpec {
    fn default() -> Self {
        Self {
            letter: 'C',
            kind: "FIXED".to_string(),
            filesystem: "NTFS".to_string(),
            label: String::new(),
            serial: 0x1A2B_3C4D,
            total_mb: 120_000,
            free_mb: 64_000,
            ready: true,
        }
    }
}

impl DriveSpec {
    /// A fixed NTFS drive with the given letter and nominal sizes.
    pub fn fixed(letter: char, total_mb: u64, free_mb: u64) -> Self {
        Self {
            letter: letter.to_ascii_uppercase(),
            total_mb,
            free_mb,
            ..Self::default()
        }
    }

    /// The `C:\`-style root path.
    pub fn root(&self) -> String {
        format!("{}:\\", self.letter.to_ascii_uppercase())
    }
}

/// How the emulated registry is backed, so a version/architecture change can
/// rebuild the matching store.
#[derive(Debug, Clone)]
enum RegistryKind {
    /// A file-backed store at this path (the default).
    File(PathBuf),
    /// A purely in-memory store.
    Memory,
    /// A store the embedder installed; never rebuilt behind their back.
    Custom,
}

/// The Windows emulation layer.
pub struct WindowsEmulation {
    /// Whether the layer answers anything at all.
    enabled: bool,
    version: WindowsVersion,
    arch: WindowsArch,
    /// The emulated directory layout.
    paths: WindowsPaths,
    /// Whether the directory macros shadow the portable (host) ones.
    emulate_paths: bool,
    registry: Box<dyn RegistryStore>,
    /// Which store `registry` is, so the seed can be rebuilt on a version
    /// change without discarding a custom one.
    registry_kind: RegistryKind,
    /// Clipboard backing file; a relative path is resolved at call time.
    clipboard: PathBuf,
    drives: Vec<DriveSpec>,
    /// Allocated `DllStruct`s, addressed by 1-based handle.
    structs: Vec<Option<DllStruct>>,
    origin: Instant,
}

impl Default for WindowsEmulation {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsEmulation {
    /// A Windows 10 / x64 emulation with a seeded registry and a `C:` drive.
    ///
    /// The registry is a [`FileRegistry`] on `.au3_registry` in the working
    /// directory: nothing is created until the script writes a value, and a
    /// value it does write outlives the run. Use
    /// [`with_memory_registry`](Self::with_memory_registry) for a store that
    /// leaves no file behind, or [`with_registry_file`](Self::with_registry_file)
    /// to choose the path.
    pub fn new() -> Self {
        let arch = WindowsArch::default();
        let version = DEFAULT_VERSION;
        let paths = WindowsPaths::new(&host_user(), &host_computer(), arch);
        let registry_path = PathBuf::from(DEFAULT_REGISTRY_FILE);
        let registry = Box::new(FileRegistry::seeded(version, arch, &paths, &registry_path));
        Self {
            enabled: true,
            version,
            arch,
            paths,
            emulate_paths: true,
            registry,
            registry_kind: RegistryKind::File(registry_path),
            clipboard: PathBuf::from(DEFAULT_CLIPBOARD_FILE),
            drives: vec![DriveSpec::default()],
            structs: Vec::new(),
            origin: Instant::now(),
        }
    }

    /// Build from the environment: `AU3_WIN_VERSION`, `AU3_WIN_ARCH`,
    /// `AU3_WIN_REGISTRY` and `AU3_WIN_EMU`. Anything unset or unrecognised
    /// falls back to the defaults (Windows 10, x64, `.au3_registry`, enabled).
    pub fn from_env() -> Self {
        let mut emu = Self::new();
        if let Ok(raw) = std::env::var(VERSION_ENV) {
            if let Some(v) = WindowsVersion::from_name(&raw) {
                emu = emu.with_version(v);
            }
        }
        if let Ok(raw) = std::env::var(ARCH_ENV) {
            if let Some(a) = WindowsArch::from_name(&raw) {
                emu = emu.with_arch(a);
            }
        }
        if let Ok(raw) = std::env::var(REGISTRY_ENV) {
            if !raw.trim().is_empty() {
                emu = emu.with_registry_file(raw.trim());
            }
        }
        if let Ok(raw) = std::env::var(ENABLE_ENV) {
            emu.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off" | "none" | "disabled"
            );
        }
        emu
    }

    /// Select the emulated Windows release (default Windows 10).
    ///
    /// Also re-seeds the registry unless a custom [`RegistryStore`] was
    /// installed with [`with_registry`](Self::with_registry).
    pub fn with_version(mut self, version: WindowsVersion) -> Self {
        self.version = version;
        self.reseed();
        self
    }

    /// Select the emulated architecture (default x64).
    pub fn with_arch(mut self, arch: WindowsArch) -> Self {
        self.arch = arch;
        self.paths = WindowsPaths::new(&self.paths.user_name, &self.paths.computer_name, arch);
        self.reseed();
        self
    }

    /// Point the file-backed registry at `path`.
    ///
    /// The store is rebuilt around the new path and the per-version seed; an
    /// existing file at `path` is overlaid on top of it.
    pub fn with_registry_file(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        self.registry_kind = RegistryKind::File(path);
        self.reseed();
        self
    }

    /// Use an in-memory registry instead of the default file-backed one.
    ///
    /// Handy for tests and for analysis runs that must leave no trace on disk;
    /// writes are then lost when the emulation is dropped.
    pub fn with_memory_registry(mut self) -> Self {
        self.registry_kind = RegistryKind::Memory;
        self.reseed();
        self
    }

    /// Install a custom registry backing store.
    ///
    /// A custom store is never rebuilt by
    /// [`with_version`](Self::with_version)/
    /// [`with_arch`](Self::with_arch)/[`with_registry_file`](Self::with_registry_file),
    /// so the order of these calls does not matter.
    pub fn with_registry(mut self, store: Box<dyn RegistryStore>) -> Self {
        self.registry = store;
        self.registry_kind = RegistryKind::Custom;
        self
    }

    /// Use `path` for the emulated clipboard instead of `.au3_clipboard`.
    pub fn with_clipboard_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.clipboard = path.into();
        self
    }

    /// Replace the emulated drive list.
    pub fn with_drives(mut self, drives: Vec<DriveSpec>) -> Self {
        self.drives = drives;
        self
    }

    /// Let the portable layer answer the directory macros, so `@TempDir` and
    /// friends stay usable host paths. The Windows-only ones (`@WindowsDir`,
    /// `@SystemDir`, ...) are still emulated.
    pub fn with_host_paths(mut self) -> Self {
        self.emulate_paths = false;
        self
    }

    /// Switch the whole layer off (it then answers nothing).
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Whether the layer will answer anything.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The selected Windows release.
    pub fn version(&self) -> WindowsVersion {
        self.version
    }

    /// The selected architecture.
    pub fn arch(&self) -> WindowsArch {
        self.arch
    }

    /// The emulated directory layout.
    pub fn paths(&self) -> &WindowsPaths {
        &self.paths
    }

    /// The registry backing store.
    pub fn registry(&self) -> &dyn RegistryStore {
        self.registry.as_ref()
    }

    /// Rebuild the seeded registry after a version/architecture/path change.
    ///
    /// A custom store is left untouched; the file store keeps the file (the
    /// seed is laid down first and the file's records are overlaid on it).
    fn reseed(&mut self) {
        self.registry = match &self.registry_kind {
            RegistryKind::File(path) => Box::new(FileRegistry::seeded(
                self.version,
                self.arch,
                &self.paths,
                path.clone(),
            )),
            RegistryKind::Memory => {
                Box::new(MemoryRegistry::seeded(self.version, self.arch, &self.paths))
            }
            RegistryKind::Custom => return,
        };
    }

    // ----- DllStruct handles -----

    fn push_struct(&mut self, s: DllStruct) -> i64 {
        // Reuse a freed slot so handles stay small, like the file table.
        if let Some(i) = self.structs.iter().position(|slot| slot.is_none()) {
            self.structs[i] = Some(s);
            return i as i64 + 1;
        }
        self.structs.push(Some(s));
        self.structs.len() as i64
    }

    fn struct_mut(&mut self, handle: i64) -> Option<&mut DllStruct> {
        if handle < 1 {
            return None;
        }
        self.structs.get_mut(handle as usize - 1)?.as_mut()
    }

    fn struct_ref(&self, handle: i64) -> Option<&DllStruct> {
        if handle < 1 {
            return None;
        }
        self.structs.get(handle as usize - 1)?.as_ref()
    }

    // ----- DllCall -----

    /// Emulate `DllCall(dll, rettype, function, type1, arg1, ...)`.
    fn dll_call(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let function = arg_str(args, 2);
        let extra: &[Value] = if args.len() > 3 { &args[3..] } else { &[] };
        let result = match function.to_ascii_lowercase().as_str() {
            "getversionexw" => self.fill_version_struct(extra),
            // The ANSI entry point fills the same fields; string fields that
            // the definition declared `char` are written narrow automatically.
            "getversionexa" | "getversionex" => self.fill_version_struct(extra),
            "rtlgetversion" => self.fill_version_struct(extra),
            "getversion" => Some(Value::Int(self.version.packed_get_version() as i64)),
            "getsysteminfo" | "getnativesysteminfo" => self.fill_system_info(extra),
            "getlasterror" => Some(Value::Int(0)),
            "setlasterror" => Some(Value::Int(0)),
            "getcurrentprocessid" => Some(Value::Int(std::process::id() as i64)),
            "getcurrentthreadid" => Some(Value::Int(std::process::id() as i64)),
            "getcurrentprocess" => Some(Value::Int(-1)),
            "gettickcount" | "gettickcount64" => {
                Some(Value::Int(self.origin.elapsed().as_millis() as i64))
            }
            _ => None,
        };
        match result {
            Some(value) => {
                ctx.set_error(0, 0);
                value
            }
            // Not emulated: hand control back to the script's error handling
            // rather than inventing a result.
            None => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    /// Write `OSVERSIONINFO(W/EX)` fields into the struct argument.
    fn fill_version_struct(&mut self, extra: &[Value]) -> Option<Value> {
        let handle = struct_handle_arg(extra)?;
        let version = self.version;
        let s = self.struct_mut(handle)?;
        if let Some(i) = s.field_alias(&["osversioninfosize", "dwosversioninfosize"]) {
            let size = s.size() as u64;
            s.set_int(i, size);
        }
        if let Some(i) = s.field_alias(&["majorversion", "dwmajorversion"]) {
            s.set_int(i, version.major() as u64);
        }
        if let Some(i) = s.field_alias(&["minorversion", "dwminorversion"]) {
            s.set_int(i, version.minor() as u64);
        }
        if let Some(i) = s.field_alias(&["buildnumber", "dwbuildnumber"]) {
            s.set_int(i, version.build() as u64);
        }
        if let Some(i) = s.field_alias(&["platformid", "dwplatformid"]) {
            s.set_int(i, version.platform_id() as u64);
        }
        if let Some(i) = s.field_alias(&["csdversion", "szcsdversion"]) {
            s.set_string(i, None, version.service_pack());
        }
        if let Some(i) = s.field_alias(&["servicepackmajor", "wservicepackmajor"]) {
            s.set_int(i, version.service_pack_major() as u64);
        }
        if let Some(i) = s.field_alias(&["servicepackminor", "wservicepackminor"]) {
            s.set_int(i, version.service_pack_minor() as u64);
        }
        if let Some(i) = s.field_alias(&["suitemask", "wsuitemask"]) {
            s.set_int(i, version.suite_mask() as u64);
        }
        if let Some(i) = s.field_alias(&["producttype", "wproducttype"]) {
            s.set_int(i, version.product_type() as u64);
        }
        Some(Value::Int(1))
    }

    /// Write `SYSTEM_INFO` fields into the struct argument.
    fn fill_system_info(&mut self, extra: &[Value]) -> Option<Value> {
        let handle = struct_handle_arg(extra)?;
        let arch = self.arch;
        let s = self.struct_mut(handle)?;
        let x64 = arch.pointer_size() == 8;
        let set = |s: &mut DllStruct, aliases: &[&str], value: u64| {
            if let Some(i) = s.field_alias(aliases) {
                s.set_int(i, value);
            }
        };
        set(s, &["wprocessorarchitecture", "processorarchitecture"], arch.system_info_id() as u64);
        set(s, &["dwpagesize", "pagesize"], 4096);
        set(
            s,
            &["lpminimumapplicationaddress", "dwminapplicationaddress"],
            0x1_0000,
        );
        set(
            s,
            &["lpmaximumapplicationaddress", "dwmaxapplicationaddress"],
            0x7FFF_FFFE_FFFF,
        );
        set(s, &["dwactiveprocessormask", "activeprocessormask"], 0xF);
        set(s, &["dwnumberofprocessors", "numberofprocessors"], 4);
        set(
            s,
            &["dwprocessortype", "processortype"],
            if x64 { 8664 } else { 586 },
        );
        set(
            s,
            &["dwallocationgranularity", "allocationgranularity"],
            65536,
        );
        set(s, &["wprocessorlevel", "processorlevel"], 6);
        set(s, &["wprocessorrevision", "processorrevision"], 0x3A09);
        Some(Value::Int(1))
    }

    // ----- registry -----

    fn reg_read(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let key = arg_str(args, 0);
        let value = arg_str(args, 1);
        match self.registry.read(&key, &value) {
            Some(data) => {
                ctx.set_error(0, 0);
                data.to_value()
            }
            None => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    fn reg_write(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !writes_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let key = arg_str(args, 0);
        let value = arg_str(args, 1);
        // `RegWrite(key, value, type, data)`; a 3-argument call omits the type.
        let (type_code, data) = if args.len() >= 4 {
            (Some(args[2].to_int()), args[3].clone())
        } else {
            (None, args.get(2).cloned().unwrap_or(Value::Null))
        };
        let Some(data) = RegistryData::from_autoit(type_code, &data) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let ok = self.registry.write(&key, &value, data);
        ctx.set_error(if ok { 0 } else { 1 }, 0);
        Value::Int(i64::from(ok))
    }

    fn reg_delete(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !writes_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let key = arg_str(args, 0);
        let value = arg_str(args, 1);
        let ok = if value.trim().is_empty() {
            self.registry.delete_key(&key, false)
        } else {
            self.registry.delete_value(&key, &value)
        };
        ctx.set_error(if ok { 0 } else { 1 }, 0);
        Value::Int(i64::from(ok))
    }

    fn reg_enum(&self, args: &[Value], ctx: &mut dyn HostContext, keys: bool) -> Value {
        let key = arg_str(args, 0);
        let instance = arg_int(args, 1);
        let names = if keys {
            self.registry.enum_keys(&key)
        } else {
            self.registry.enum_values(&key)
        };
        if instance >= 1 && (instance as usize) <= names.len() {
            ctx.set_error(0, 0);
            Value::Str(names[instance as usize - 1].clone())
        } else {
            ctx.set_error(1, 0);
            Value::str("")
        }
    }

    // ----- clipboard -----

    /// The clipboard file, resolving a relative path against the working dir.
    fn clipboard_path(&self) -> PathBuf {
        if self.clipboard.is_absolute() {
            self.clipboard.clone()
        } else {
            std::env::current_dir()
                .map(|d| d.join(&self.clipboard))
                .unwrap_or_else(|_| self.clipboard.clone())
        }
    }

    fn clip_get(&self, ctx: &mut dyn HostContext) -> Value {
        let path = self.clipboard_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                ctx.set_error(0, 0);
                Value::Str(text)
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    fn clip_put(&self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !writes_allowed(ctx) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let text = arg_str(args, 0);
        let ok = std::fs::write(self.clipboard_path(), text).is_ok();
        ctx.set_error(if ok { 0 } else { 1 }, 0);
        Value::Int(i64::from(ok))
    }

    // ----- drives -----

    fn find_drive(&self, path: &str) -> Option<&DriveSpec> {
        let letter = path
            .trim()
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase())?;
        self.drives
            .iter()
            .find(|d| d.letter.to_ascii_uppercase() == letter)
    }

    fn drive_get_drive(&self, args: &[Value]) -> Value {
        let wanted = arg_str(args, 0).to_ascii_uppercase();
        let wanted = if wanted.is_empty() {
            "ALL".to_string()
        } else {
            wanted
        };
        let mut out: Vec<Value> = Vec::new();
        for d in &self.drives {
            if wanted == "ALL" || d.kind.eq_ignore_ascii_case(&wanted) {
                out.push(Value::Str(d.root()));
            }
        }
        Value::array(out)
    }

    fn drive_field<F>(&self, args: &[Value], ctx: &mut dyn HostContext, f: F) -> Value
    where
        F: Fn(&DriveSpec) -> Value,
    {
        let path = arg_str(args, 0);
        match self.find_drive(&path) {
            Some(d) => {
                ctx.set_error(0, 0);
                f(d)
            }
            None => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }
}

impl Platform for WindowsEmulation {
    fn name(&self) -> &'static str {
        "windows-emulation"
    }

    fn provides(&self, name: &str) -> bool {
        self.enabled && FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    fn macro_value(&self, name: &str) -> Option<Value> {
        if !self.enabled {
            return None;
        }
        macro_value(self, name)
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        if !self.enabled {
            return Ok(None);
        }
        let key = name.to_ascii_lowercase();
        let value = match key.as_str() {
            // ---------------- DllStruct ----------------
            "dllstructcreate" => {
                let definition = arg_str(&args, 0);
                match DllStruct::create(&definition, self.arch) {
                    Ok(s) => {
                        let handle = self.push_struct(s);
                        ctx.set_error(0, 0);
                        Value::Int(handle)
                    }
                    Err(_) => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "dllstructgetsize" => {
                let handle = arg_int(&args, 0);
                match self.struct_ref(handle) {
                    Some(s) => {
                        let size = s.size() as i64;
                        ctx.set_error(0, 0);
                        Value::Int(size)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "dllstructgetptr" => {
                let handle = arg_int(&args, 0);
                if self.struct_ref(handle).is_some() {
                    ctx.set_error(0, 0);
                    Value::Int(handle)
                } else {
                    ctx.set_error(1, 0);
                    Value::Int(0)
                }
            }
            "isdllstruct" => {
                let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
                Value::Int(i64::from(self.struct_ref(handle).is_some()))
            }
            "dllstructgetdata" | "dllstructsetdata" => {
                let handle = arg_int(&args, 0);
                let selector = args
                    .get(1)
                    .map(FieldSelector::from_value)
                    .unwrap_or(FieldSelector::Index(0));
                // `DllStructGetData(Struct, Element [, Index])` and
                // `DllStructSetData(Struct, Element, Value [, Index])` put the
                // array index in different positions.
                let is_get = key == "dllstructgetdata";
                let element = args
                    .get(if is_get { 2 } else { 3 })
                    .map(|v| v.to_int())
                    .filter(|n| *n >= 1);
                let Some(s) = self.struct_mut(handle) else {
                    ctx.set_error(1, 0);
                    return Ok(Some(if is_get {
                        Value::str("")
                    } else {
                        Value::Int(0)
                    }));
                };
                let Some(field) = s.field_index(&selector) else {
                    ctx.set_error(1, 0);
                    return Ok(Some(if is_get {
                        Value::str("")
                    } else {
                        Value::Int(0)
                    }));
                };
                if is_get {
                    match s.get(field, element.map(|e| e as usize)) {
                        Some(v) => {
                            ctx.set_error(0, 0);
                            v
                        }
                        None => {
                            ctx.set_error(1, 0);
                            Value::str("")
                        }
                    }
                } else {
                    let value = args.get(2).cloned().unwrap_or(Value::Null);
                    let ok = s.set(field, element.map(|e| e as usize), &value);
                    ctx.set_error(if ok { 0 } else { 1 }, 0);
                    Value::Int(i64::from(ok))
                }
            }
            // ---------------- DllCall ----------------
            "dllcall" => self.dll_call(&args, ctx),
            // ---------------- registry ----------------
            "regread" => self.reg_read(&args, ctx),
            "regwrite" => self.reg_write(&args, ctx),
            "regdelete" => self.reg_delete(&args, ctx),
            "regenumkey" => self.reg_enum(&args, ctx, true),
            "regenumval" => self.reg_enum(&args, ctx, false),
            // ---------------- clipboard ----------------
            "clipget" => self.clip_get(ctx),
            "clipput" => self.clip_put(&args, ctx),
            // ---------------- drives ----------------
            "drivegetdrive" => self.drive_get_drive(&args),
            "drivegettype" => self.drive_field(&args, ctx, |d| Value::Str(d.kind.clone())),
            "drivegetfilesystem" => {
                self.drive_field(&args, ctx, |d| Value::Str(d.filesystem.clone()))
            }
            "drivegetlabel" => self.drive_field(&args, ctx, |d| Value::Str(d.label.clone())),
            "drivegetserial" => {
                self.drive_field(&args, ctx, |d| Value::Int(d.serial as i64))
            }
            "drivespacetotal" => {
                self.drive_field(&args, ctx, |d| Value::Int(d.total_mb as i64))
            }
            "drivespacefree" => self.drive_field(&args, ctx, |d| Value::Int(d.free_mb as i64)),
            "drivestatus" => self.drive_field(&args, ctx, |d| {
                Value::Str(if d.ready { "READY" } else { "NOTREADY" }.to_string())
            }),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}

/// The environment-dependent macros the emulation answers.
fn macro_value(emu: &WindowsEmulation, name: &str) -> Option<Value> {
    let p = &emu.paths;
    let version = emu.version;
    let arch = emu.arch;
    let lower = name.to_ascii_lowercase();
    // Windows-only directories are always emulated; the host-overlapping ones
    // (`@TempDir`, `@AppDataDir`, ...) only when path emulation is on, so an
    // embedder can keep usable host paths with `with_host_paths()`.
    let host_shared = matches!(
        lower.as_str(),
        "tempdir"
            | "desktopdir"
            | "appdatadir"
            | "localappdatadir"
            | "userprofiledir"
            | "homepath"
            | "mydocumentsdir"
    );
    if host_shared && !emu.emulate_paths {
        return None;
    }
    let value = match lower.as_str() {
        // ----- OS identity -----
        "osversion" => Value::Str(version.os_version_macro().to_string()),
        "ostype" => Value::Str("WIN32_NT".to_string()),
        "osbuild" => Value::Str(version.build().to_string()),
        "osservicepack" => Value::Str(version.service_pack().to_string()),
        "osarch" | "processorarch" | "cpuch" => Value::Str(arch.as_str().to_string()),
        "oslang" | "oslocale" | "muilang" => Value::Str("0409".to_string()),
        "kblayout" => Value::Str("00000409".to_string()),
        "autoitx64" => Value::Int(i64::from(arch.pointer_size() == 8)),
        // ----- machine identity -----
        "computername" => Value::Str(p.computer_name.clone()),
        "username" => Value::Str(p.user_name.clone()),
        "logondomain" => Value::Str(p.computer_name.clone()),
        "logondnsdomain" => Value::Str(String::new()),
        "logonserver" => Value::Str(format!(r"\\{}", p.computer_name)),
        "comspec" => Value::Str(format!(r"{}\cmd.exe", p.windows_dir)),
        // ----- directories -----
        "windowsdir" => Value::Str(p.windows_dir.clone()),
        "systemdir" => Value::Str(p.system_dir.clone()),
        "systemx86dir" => Value::Str(p.system_x86_dir.clone()),
        "programfilesdir" => Value::Str(p.program_files.clone()),
        "commonfilesdir" => Value::Str(p.common_files.clone()),
        "appdatacommondir" => Value::Str(p.program_data.clone()),
        "homedrive" => Value::Str(p.home_drive.clone()),
        "homeshare" => Value::Str(p.home_share()),
        "homepath" | "userprofiledir" => Value::Str(p.user_profile.clone()),
        "tempdir" => Value::Str(p.temp()),
        "desktopdir" => Value::Str(p.desktop()),
        "desktopcommondir" => Value::Str(format!(r"{}\Public\Desktop", drive_root(&p.home_drive))),
        "mydocumentsdir" => Value::Str(p.documents()),
        "documentscommondir" => {
            Value::Str(format!(r"{}\Public\Documents", drive_root(&p.home_drive)))
        }
        "mymusicdir" => Value::Str(format!(r"{}\Music", p.user_profile)),
        "mypicsdir" => Value::Str(format!(r"{}\Pictures", p.user_profile)),
        "myvideosdir" => Value::Str(format!(r"{}\Videos", p.user_profile)),
        "favoritesdir" => Value::Str(format!(r"{}\Favorites", p.user_profile)),
        "favoritescommondir" => {
            Value::Str(format!(r"{}\Microsoft\Windows\Favorites", p.program_data))
        }
        "fontsdir" => Value::Str(format!(r"{}\Fonts", p.windows_dir)),
        "appdatadir" => Value::Str(p.appdata()),
        "localappdatadir" => Value::Str(p.local_appdata()),
        "startmenudir" => Value::Str(p.start_menu()),
        "startmenucommondir" => Value::Str(p.start_menu_common()),
        "programsdir" => Value::Str(format!(r"{}\Programs", p.start_menu())),
        "programscommondir" => Value::Str(format!(r"{}\Programs", p.start_menu_common())),
        "startupdir" => Value::Str(format!(r"{}\Programs\Startup", p.start_menu())),
        "startupcommondir" => {
            Value::Str(format!(r"{}\Programs\Startup", p.start_menu_common()))
        }
        "recentdir" => Value::Str(p.recent()),
        "sendtodir" => Value::Str(p.send_to()),
        "templatesdir" => Value::Str(p.templates()),
        "internetcachedir" => Value::Str(p.internet_cache()),
        _ => return None,
    };
    Some(value)
}

/// `C:` → `C:\` so the common-profile paths can be built from the drive.
fn drive_root(home_drive: &str) -> String {
    format!("{}\\", home_drive.trim_end_matches('\\'))
}

/// Find a struct handle among `DllCall`'s type/value argument pairs.
fn struct_handle_arg(extra: &[Value]) -> Option<i64> {
    extra.iter().find_map(|v| match v {
        Value::Int(i) if *i >= 1 => Some(*i),
        _ => None,
    })
}

/// Argument as a string, using AutoIt's coercion.
fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

/// Argument as an integer.
fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

/// Whether the execution profile allows side effects.
fn writes_allowed(ctx: &dyn HostContext) -> bool {
    matches!(ctx.profile().effects, EffectPolicy::Allow)
}

/// The account name to present, from the host environment.
fn host_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "User".to_string())
}

/// The computer name to present, from the host environment.
fn host_computer() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "DESKTOP-EMULATED".to_string())
}
