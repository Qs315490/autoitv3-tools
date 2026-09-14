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
//! | drives | `DriveGet*`, `DriveSpace*` against a configurable [`DriveSpec`] list, plus the emulated `DriveMapAdd`/`DriveMapDel`/`DriveMapGet`/`DriveSetLabel` mappings |
//! | Windows files | `FileGetVersion` (PE `RT_VERSION`), `FileCreateShortcut`/`FileGetShortcut` (`.lnk`), `FileCreateNTFSLink`, `FileRecycle`/`FileRecycleEmpty`, `FileInstall` |
//! | callbacks | `DllCallbackRegister`/`DllCallbackGetPtr`/`DllCallbackFree` hand out synthetic pointers; `DllCallAddress` has no routine behind it and fails predictably |
//! | system info | `MemGetStats` (a fixed machine profile, so runs are reproducible) and `IsAdmin` |
//! | shell | `ShellExecute`/`ShellExecuteWait`/`RunAs`/`RunAsWait` launch host processes; `Shutdown` only records the request |
//! | COM | no runtime: `ObjCreate`/`ObjGet`/… fail with `@error = 1` and `IsObj` is `0`, rather than inventing objects |
//! | GUI | `GUICreate`/`GUICtrlCreate*`/`GUICtrlSet*`/`GUIGetMsg`/`Win*`/`Control*`/dialogs/tray/input over an in-memory widget model ([`gui`]); rendering and events come from a pluggable [`GuiBackend`], headless by default |
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
//! Real Win32 behaviour. There is no PE loader, no COM, no real `DllCall`: a
//! struct is a `Vec<u8>` this layer owns, an unimplemented `DllCall` sets
//! `@error = 1` and returns `0` rather than inventing a result, and the COM
//! builtins fail the same way. Callback pointers are registered but never
//! invoked by native code, and the GUI is **emulated, not rendered**: the
//! default [`GuiBackend`] draws nothing, so nothing appears on screen until a
//! real backend (the optional egui crate) is installed.
//! That keeps a script's own error handling in charge — and when you would
//! rather stop at the boundary, install [`crate::host_platform`] without this
//! layer (set `AU3_WIN_EMU=0`, or use `--no-win-emu`).

mod compress;
mod crypto;
pub mod gui;
mod paths;
mod registry;
mod shell;
mod version;

pub use gui::{
    Control, ControlKind, GuiBackend, GuiEvent, GuiImage, GuiUpdate, HeadlessBackend, Window,
    WindowState,
};
pub use paths::WindowsPaths;
pub use crypto::{CipherAlg, HashAlg};
pub use crate::winfmt::{PeImage, Resource, Selector};
pub use crate::winfmt::{DllStruct, FieldSelector, Shortcut};
pub use registry::{FileRegistry, MemoryRegistry, RegistryData, RegistryStore};
pub use version::WindowsVersion;
pub use crate::winfmt::WindowsArch;

use std::collections::BTreeMap;
use std::cell::RefCell;
use std::path::PathBuf;
use std::path::Path;
use std::rc::Rc;
use std::time::{Instant, SystemTime};

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::EffectKind;
use autoitv3_runtime::value::{NativeObject, ObjRef, Value};

/// Environment variable selecting the emulated version (`win10`, `win11`, ...).
pub const VERSION_ENV: &str = "AU3_WIN_VERSION";
/// Environment variable selecting the emulated architecture (`x64`, `x86`).
pub const ARCH_ENV: &str = "AU3_WIN_ARCH";
/// Set this to `0`/`false`/`off` to leave the layer out of the stack.
pub const ENABLE_ENV: &str = "AU3_WIN_EMU";
/// Environment variable naming the file the emulated registry lives in.
pub const REGISTRY_ENV: &str = "AU3_WIN_REGISTRY";
/// Environment variable naming the PE image whose resources the emulated
/// `FindResourceW`/`LoadResource` answer from — the `.exe` the script was
/// compiled into. Usually unnecessary: a sibling image is found automatically
/// (see [`find_resource_module`]).
pub const RESOURCE_MODULE_ENV: &str = "AU3_RESOURCE_MODULE";
/// Set this to report every `DllCall` target the emulation does not implement
/// (once each, on stderr). Handy for finding the next boundary to fill in.
pub const TRACE_ENV: &str = "AU3_WINEMU_TRACE";
/// The version used when nothing selects one.
pub const DEFAULT_VERSION: WindowsVersion = WindowsVersion::Win10;

/// The directories searched for resources: the script's own directory first,
/// then the working directory.
///
/// `script` is the path of the `.au3` being analysed, when the caller knows it.
pub fn resource_search_dirs(script: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = script.and_then(|p| p.parent()) {
        if !dir.as_os_str().is_empty() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if !dirs.contains(&cwd) {
            dirs.push(cwd);
        }
    }
    dirs
}

/// Find the PE image whose resources should answer `FindResourceW`.
///
/// A script that was compiled into an `.exe` keeps its payload in that image's
/// resources, and the image usually sits right next to the script, so nothing
/// has to be configured in the common case. [`resource_search_dirs`] gives the
/// directories; see [`PeImage::find_resource_module`] for how the image is
/// chosen within one.
pub fn find_resource_module(script: Option<&std::path::Path>) -> Option<PathBuf> {
    let stem = script
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str());
    resource_search_dirs(script)
        .iter()
        .find_map(|dir| PeImage::find_resource_module(dir, stem))
}

/// Whether any search directory holds resources staged as files.
///
/// `AutoIt3Wrapper_Res_File_Add` writes them with a `__` prefix (`__NAME`) or
/// under `__Res64`/`__ResImage`, so that is what to look for.
pub fn has_staged_resources(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        entries.flatten().any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("__"))
        })
    })
}
/// The clipboard file, relative to the working directory.
pub const DEFAULT_CLIPBOARD_FILE: &str = ".au3_clipboard";
/// The registry file, relative to the working directory.
pub const DEFAULT_REGISTRY_FILE: &str = ".au3_registry";
/// The directory `FileRecycle` moves files into, relative to the working
/// directory.
pub const DEFAULT_RECYCLE_DIR: &str = ".au3_recycle";
/// Set this to `0`/`false`/`off` to make `IsAdmin` report a standard user.
pub const ADMIN_ENV: &str = "AU3_WIN_ADMIN";
/// The first synthetic pointer `DllCallbackRegister` hands out.
const CALLBACK_BASE: i64 = 0x0050_0000;

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
    "DllOpen",
    "DllClose",
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
    "DriveGetFileSystem",
    "DriveGetLabel",
    "DriveGetSerial",
    "DriveSpaceTotal",
    "DriveSpaceFree",
    "DriveStatus",
    // Windows files / PE resources
    "FileGetVersion",
    "FileCreateShortcut",
    "FileGetShortcut",
    "FileCreateNTFSLink",
    "FileRecycle",
    "FileRecycleEmpty",
    "FileInstall",
    // native calls: address + callbacks
    "DllCallAddress",
    "DllCallbackRegister",
    "DllCallbackGetPtr",
    "DllCallbackFree",
    // COM — no runtime off Windows, so these fail predictably
    "ObjCreate",
    "ObjCreateInterface",
    "ObjEvent",
    "ObjGet",
    "ObjName",
    "IsObj",
    // system information
    "MemGetStats",
    "IsAdmin",
    // drive mappings
    "DriveMapAdd",
    "DriveMapDel",
    "DriveMapGet",
    "DriveSetLabel",
    // shell execution
    "ShellExecute",
    "ShellExecuteWait",
    "RunAs",
    "RunAsWait",
    "Shutdown",
];

/// The image base a PE is loaded at, as `GetModuleHandleW` reports it.
const EMULATED_IMAGE_BASE: i64 = 0x0040_0000;

/// A resource `FindResourceW` handed out, plus the address `LockResource`
/// materialised its bytes at.
#[derive(Debug, Clone)]
struct ResourceHandle {
    data: Vec<u8>,
    address: Option<u64>,
}

/// What an emulated `DllCall` produced.
///
/// AutoIt's `DllCall` returns an array: element 0 is the function's return
/// value and elements 1..n are the arguments (the obfuscator reads its
/// out-parameters straight out of there — `$r[5]` for the fifth argument), so
/// an emulated call reports its by-ref results the same way.
/// One live pseudo-COM object (`ObjCreate` on the emulation).
struct PseudoObject {
    kind: PseudoKind,
    /// `Scripting.Dictionary` storage (case-insensitive keys).
    dict: BTreeMap<String, Value>,
}

/// The ProgIDs the emulation answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PseudoKind {
    Dictionary,
    WScriptShell,
    FileSystemObject,
}

impl PseudoKind {
    fn from_progid(progid: &str) -> Option<PseudoKind> {
        let p = progid.trim().to_ascii_lowercase();
        Some(match p.as_str() {
            "scripting.dictionary" => PseudoKind::Dictionary,
            "wscript.shell" | "wscript.shell.1" => PseudoKind::WScriptShell,
            "scripting.filesystemobject" => PseudoKind::FileSystemObject,
            _ => return None,
        })
    }

}

/// An open handle from the emulated `CreateFileW`.
struct OpenFile {
    path: String,
    content: Vec<u8>,
    write: bool,
    pos: u64,
}

/// Windows-path normalisation for the file sandbox: backslashes, lower-case.
fn normalise_sandbox_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

struct DllOutcome {
    retval: Value,
    /// `(argument index, value after the call)`.
    writes: Vec<(usize, Value)>,
}

impl DllOutcome {
    /// A call with no by-ref results.
    fn value(retval: Value) -> Self {
        Self {
            retval,
            writes: Vec::new(),
        }
    }

    /// A call that wrote `value` into argument `index`.
    fn with(retval: Value, index: usize, value: Value) -> Self {
        Self {
            retval,
            writes: vec![(index, value)],
        }
    }
}

/// CryptoAPI state: the hash and key objects `CryptCreateHash` /
/// `CryptDeriveKey` hand out.
#[derive(Debug, Default)]
struct CryptoState {
    hashes: Vec<Option<HashObject>>,
    keys: Vec<Option<KeyObject>>,
}

#[derive(Debug)]
struct HashObject {
    alg: crypto::HashAlg,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct KeyObject {
    alg: crypto::CipherAlg,
    key: Vec<u8>,
    /// The IV `CryptDeriveKey` produced, readable via `CryptGetKeyParam(KP_IV)`.
    iv: Vec<u8>,
}

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
    /// What `DriveGetFileSystem` reports, e.g. `NTFS`.
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
    /// Whether the directory macros shadow the common (host) ones.
    emulate_paths: bool,
    registry: Box<dyn RegistryStore>,
    /// Which store `registry` is, so the seed can be rebuilt on a version
    /// change without discarding a custom one.
    registry_kind: RegistryKind,
    /// Clipboard backing file; a relative path is resolved at call time.
    clipboard: PathBuf,
    /// Cached clipboard state so `ClipGet` polls do not re-read the file (and
    /// re-run `current_dir`) every call: resolved path, its mtime at last
    /// read, and the text.
    clipboard_cache: std::cell::RefCell<Option<(PathBuf, Option<SystemTime>, String)>>,
    drives: Vec<DriveSpec>,
    /// Allocated `DllStruct`s, addressed by 1-based handle.
    structs: Vec<Option<DllStruct>>,
    /// The PE file whose resources the module/resource calls answer from.
    module: Option<PeImage>,
    /// Where `module` was loaded from, for reporting.
    module_path: Option<PathBuf>,
    /// Directories searched for resources staged as files (see
    /// [`PeImage::find_resource_file`]) before the PE image is consulted.
    resource_dirs: Vec<PathBuf>,
    /// Resources handed out by `FindResourceW`/`LoadResource`, 1-based.
    handles: Vec<Option<ResourceHandle>>,
    /// Resource bytes materialised by `LockResource`, keyed by their address.
    /// Shared like a struct's storage so `DllStructCreate` can map over them.
    blobs: Vec<(u64, Rc<RefCell<Vec<u8>>>)>,
    /// In-memory sandbox backing the emulated file APIs (`CreateFileW` & co):
    /// normalised path -> content. Seeded via `with_file`, mutated by
    /// `WriteFile`; the real filesystem is never touched.
    sandbox_files: BTreeMap<String, Vec<u8>>,
    /// Handles `CreateFileW` handed out, id -> open state.
    open_files: BTreeMap<i64, OpenFile>,
    /// Next id for `open_files`.
    next_file_handle: i64,
    /// Handles the scripted `EnumWindows` family reports (see
    /// [`WindowsEmulation::with_scripted_windows`]).
    scripted_windows: Vec<i64>,
    /// Callback invocations scheduled by the emulated enumerators, drained by
    /// the runtime after the `DllCall` returns.
    pending_callbacks: Vec<(String, Vec<Value>)>,
    /// Live pseudo-COM objects, 1-based handle = index + 1.
    pseudo_objects: Vec<Option<PseudoObject>>,
    /// Handles handed out by `DllOpen`.
    dlls: Vec<Option<String>>,
    /// Emulated CryptoAPI objects.
    crypto: CryptoState,
    /// Next synthetic address handed out (`&struct`, `LockResource`).
    next_addr: u64,
    origin: Instant,
    /// Report unimplemented `DllCall` targets on stderr (`AU3_WINEMU_TRACE`).
    trace_dll: bool,
    /// Targets already reported, so the trace stays readable.
    traced: std::collections::HashSet<String>,
    /// Script functions registered with `DllCallbackRegister`, 1-based. The
    /// pointer handed out is `CALLBACK_BASE + index * 16`.
    callbacks: Vec<Option<String>>,
    /// Emulated network drive mappings: device (`X:`) → remote share.
    drive_maps: Vec<(String, String)>,
    /// Where `FileRecycle` moves files and directories.
    recycle_dir: PathBuf,
    /// What `IsAdmin` reports.
    is_admin: bool,
    /// How many `Shutdown` requests were recorded (none is ever acted on).
    shutdowns: u32,
    /// The emulated GUI: widget model, backend and scripted events.
    gui: gui::GuiState,
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
            clipboard_cache: std::cell::RefCell::new(None),
            drives: vec![DriveSpec::default()],
            structs: Vec::new(),
            module: None,
            module_path: None,
            resource_dirs: Vec::new(),
            handles: Vec::new(),
            blobs: Vec::new(),
            sandbox_files: BTreeMap::new(),
            open_files: BTreeMap::new(),
            next_file_handle: 0x1000,
            scripted_windows: Vec::new(),
            pending_callbacks: Vec::new(),
            pseudo_objects: Vec::new(),
            dlls: Vec::new(),
            crypto: CryptoState::default(),
            next_addr: 0x0100_0000,
            origin: Instant::now(),
            trace_dll: false,
            traced: std::collections::HashSet::new(),
            callbacks: Vec::new(),
            drive_maps: Vec::new(),
            recycle_dir: PathBuf::from(DEFAULT_RECYCLE_DIR),
            is_admin: true,
            shutdowns: 0,
            gui: gui::GuiState::new(),
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
        if let Ok(raw) = std::env::var(RESOURCE_MODULE_ENV) {
            if !raw.trim().is_empty() {
                emu = emu.with_module_file(raw.trim());
            }
        }
        if let Ok(raw) = std::env::var(TRACE_ENV) {
            emu.trace_dll = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            );
        }
        if let Ok(raw) = std::env::var(ENABLE_ENV) {
            emu.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off" | "none" | "disabled"
            );
        }
        if let Ok(raw) = std::env::var(ADMIN_ENV) {
            emu.is_admin = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
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

    /// Seed the emulated file sandbox: `CreateFileW(path, GENERIC_READ)` will
    /// read these bytes, and `WriteFile` content is only visible to the
    /// emulation (the real filesystem is never touched). Paths are
    /// normalised to lower-case with backslashes.
    pub fn with_file(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        let path = normalise_sandbox_path(&path.into());
        self.sandbox_files.insert(path, contents.into());
        self
    }

    /// Script the window list the `EnumWindows` family reports. Each handle
    /// becomes one callback invocation whose first argument is the handle;
    /// the AutoIt callback function itself runs after the `DllCall` returns
    /// (the emulation never re-enters the interpreter mid-call).
    pub fn with_scripted_windows(mut self, handles: Vec<i64>) -> Self {
        self.scripted_windows = handles;
        self
    }

    /// Replace the emulated drive list.
    pub fn with_drives(mut self, drives: Vec<DriveSpec>) -> Self {
        self.drives = drives;
        self
    }

    /// Whether `IsAdmin` reports an elevated user (default `true`).
    ///
    /// `AU3_WIN_ADMIN=0` selects a standard user for
    /// [`from_env`](Self::from_env) callers.
    pub fn with_admin(mut self, admin: bool) -> Self {
        self.is_admin = admin;
        self
    }

    /// Where `FileRecycle` moves files instead of `.au3_recycle`.
    pub fn with_recycle_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.recycle_dir = path.into();
        self
    }

    /// Install a GUI rendering backend (default: headless, renders nothing).
    pub fn with_gui_backend(mut self, backend: Box<dyn GuiBackend>) -> Self {
        self.gui.set_backend(backend);
        self
    }

    /// Seed the event queue `GUIGetMsg`/`TrayGetMsg` drain.
    pub fn with_gui_events(mut self, events: Vec<GuiEvent>) -> Self {
        self.gui = self.gui.with_events(events);
        self
    }

    /// Queue answers for `InputBox` and the `File*Dialog` functions.
    pub fn with_gui_answers(mut self, answers: Vec<String>) -> Self {
        self.gui = self.gui.with_answers(answers);
        self
    }

    /// Deliver `$GUI_EVENT_CLOSE` on the *n*-th `GUIGetMsg`, so a script's
    /// message loop terminates without a user.
    pub fn with_gui_auto_close(mut self, polls: u64) -> Self {
        self.gui = self.gui.with_auto_close(polls);
        self
    }

    /// Install the optional egui **offscreen** renderer.
    ///
    /// Compiled only with the `gui-egui` feature; the backend renders the model
    /// to an RGBA buffer and can write a PNG, but opens no window.
    #[cfg(feature = "gui-egui")]
    pub fn with_egui_backend(self) -> Self {
        self.with_gui_backend(Box::new(autoitv3_gui_egui::EguiBackend::new()))
    }

    /// Capture the current GUI frame, when the installed backend can render one
    /// (the headless default cannot).
    pub fn gui_snapshot(&mut self) -> Option<GuiImage> {
        self.gui.snapshot()
    }

    /// Let the common layer answer the directory macros, so `@TempDir` and
    /// friends stay usable host paths. The Windows-only ones (`@WindowsDir`,
    /// `@SystemDir`, ...) are still emulated.
    pub fn with_host_paths(mut self) -> Self {
        self.emulate_paths = false;
        self
    }

    /// Answer `GetModuleHandleW`/`FindResourceW`/`LoadResource`/`LockResource`
    /// from the resources of this PE file.
    ///
    /// The reference sample reaches its payload through
    /// `GetModuleHandleW(NULL)` → `FindResourceW(hMod, "PAYLOAD", RT_RCDATA)` →
    /// `SizeofResource` → `LoadResource` → `LockResource`, so pointing this at
    /// the `.exe` the script was compiled from makes that path work. A file
    /// that cannot be read leaves the calls failing, as before.
    pub fn with_module_file(mut self, path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref();
        self.module_path = Some(path.to_path_buf());
        match PeImage::load(path) {
            Ok(image) => self.module = Some(image),
            Err(e) => {
                if self.trace_dll {
                    eprintln!("[winemu] cannot read module {}: {e}", path.display());
                }
                self.module = None;
            }
        }
        self
    }

    /// The loaded module image, if any.
    pub fn module(&self) -> Option<&PeImage> {
        self.module.as_ref()
    }

    /// The file the module was loaded from, if one was named or found.
    pub fn module_path(&self) -> Option<&std::path::Path> {
        self.module_path.as_deref()
    }

    /// Also look for resources staged as files in these directories, before
    /// consulting the PE image.
    ///
    /// `AutoIt3Wrapper_Res_File_Add` writes each embedded resource next to the
    /// script (`__NAME`, `__Res64/NAME`, `__ResImage/_NAME`), so this is how an
    /// analysis reads the payload without the `.exe` that carried it.
    pub fn with_resource_dirs(
        mut self,
        dirs: impl IntoIterator<Item = impl Into<PathBuf>>,
    ) -> Self {
        self.resource_dirs = dirs.into_iter().map(Into::into).collect();
        self
    }

    /// The directories searched for staged resource files.
    pub fn resource_dirs(&self) -> &[PathBuf] {
        &self.resource_dirs
    }

    /// Report every `DllCall` target the emulation does not implement, once
    /// each, on stderr.
    pub fn with_dll_trace(mut self, on: bool) -> Self {
        self.trace_dll = on;
        self
    }

    /// The `DllCall` targets seen so far that this layer does not implement.
    pub fn unimplemented_dll_calls(&self) -> Vec<String> {
        let mut out: Vec<String> = self.traced.iter().cloned().collect();
        out.sort();
        out
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

    // ----- emulated address space -----

    /// Hand out a synthetic address for `len` bytes.
    ///
    /// Only the emulation resolves these: they are handed to the script by
    /// `DllStructGetPtr` / `LockResource` and consumed by `RtlMoveMemory`.
    fn allocate(&mut self, len: usize) -> u64 {
        let base = self.next_addr;
        self.next_addr = base + ((len as u64 + 0xFFF) & !0xFFF).max(0x1000);
        base
    }

    /// Resolve a `DllStruct` by its handle *or* by an address it was given.
    fn struct_any_mut(&mut self, value: i64) -> Option<&mut DllStruct> {
        if value >= 1 {
            let index = value as usize - 1;
            if self.structs.get(index).is_some_and(|s| s.is_some()) {
                return self.structs.get_mut(index)?.as_mut();
            }
        }
        let addr = value as u64;
        let found = self.structs.iter().position(|slot| {
            slot.as_ref().is_some_and(|s| {
                s.address() != 0 && addr >= s.address() && addr < s.address() + s.size() as u64
            })
        })?;
        self.structs.get_mut(found)?.as_mut()
    }

    /// Read `len` bytes at an emulated address.
    fn memory_read(&self, addr: u64, len: usize) -> Option<Vec<u8>> {
        for s in self.structs.iter().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr + len as u64 <= base + size {
                let start = (addr - base) as usize;
                return Some(s.bytes()[start..start + len].to_vec());
            }
        }
        for (base, data) in &self.blobs {
            let data = data.borrow();
            if addr >= *base && addr + len as u64 <= *base + data.len() as u64 {
                let start = (addr - *base) as usize;
                return Some(data[start..start + len].to_vec());
            }
        }
        None
    }

    /// The shared storage an emulated address points into, with the offset of
    /// that address inside it. `DllStructCreate($def, $ptr)` maps onto this
    /// instead of allocating, so the two views stay aliases of one another.
    fn memory_storage(&self, addr: u64) -> Option<(Rc<RefCell<Vec<u8>>>, usize, u64)> {
        for s in self.structs.iter().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr < base + size {
                let (storage, offset) = s.storage();
                return Some((storage, offset + (addr - base) as usize, addr));
            }
        }
        for (base, data) in &self.blobs {
            let len = data.borrow().len() as u64;
            if addr >= *base && addr < *base + len {
                return Some((Rc::clone(data), (addr - *base) as usize, addr));
            }
        }
        None
    }

    /// Write `bytes` at an emulated address.
    fn memory_write(&mut self, addr: u64, bytes: &[u8]) -> bool {
        for s in self.structs.iter_mut().flatten() {
            let (base, size) = (s.address(), s.size() as u64);
            if base != 0 && addr >= base && addr + bytes.len() as u64 <= base + size {
                return s.write_at((addr - base) as usize, bytes);
            }
        }
        for (base, data) in &mut self.blobs {
            let len = data.borrow().len() as u64;
            if addr >= *base && addr + bytes.len() as u64 <= *base + len {
                let start = (addr - *base) as usize;
                data.borrow_mut()[start..start + bytes.len()].copy_from_slice(bytes);
                return true;
            }
        }
        false
    }

    /// The name `open_dll` handed `handle` out for.
    fn dll_name(&self, handle: i64) -> Option<&str> {
        self.dlls
            .get(handle as usize - 1)?
            .as_ref()
            .map(String::as_str)
    }

    /// Resolve a string argument that is either a literal (`"kernel32.dll"`)
    /// or a pointer into emulated memory (`DllStructGetPtr`).
    fn c_string_arg(&self, value: Value) -> Option<String> {
        match value {
            Value::Str(text) => Some(text),
            Value::Int(addr) => self.c_string_at(addr as u64, true),
            _ => None,
        }
    }

    /// Read a NUL-terminated string at an emulated address.
    ///
    /// Unit-by-unit, with the storage bounds as the hard stop: a short
    /// `DllStruct` buffer is a valid string home, and emulated memory has no
    /// pages to over-read into.
    fn c_string_at(&self, addr: u64, wide: bool) -> Option<String> {
        let mut out = String::new();
        let mut at = addr;
        if wide {
            loop {
                let b = self.memory_read(at, 2)?;
                let unit = u16::from_le_bytes([b[0], b[1]]);
                if unit == 0 {
                    return Some(out);
                }
                out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
                at += 2;
            }
        } else {
            loop {
                let b = self.memory_read(at, 1)?;
                if b[0] == 0 {
                    return Some(out);
                }
                out.push(b[0] as char);
                at += 1;
            }
        }
    }

    /// Write a NUL-terminated string into emulated memory; `max` bounds the
    /// byte footprint (buffer size semantics).
    fn write_c_string(&mut self, addr: u64, text: &str, wide: bool, max: usize) -> bool {
        let bytes: Vec<u8> = if wide {
            text.encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(|u| u.to_le_bytes())
                .take(max)
                .collect()
        } else {
            text.bytes()
                .chain(std::iter::once(0))
                .take(max)
                .collect()
        };
        self.memory_write(addr, &bytes)
    }

    // ----- pseudo COM -----

    /// `ObjCreate` for a ProgID the emulation answers. `None` = this ProgID is
    /// not simulated (the honest `@error = 1` path).
    fn pseudo_com_create(&mut self, progid: &str) -> Option<usize> {
        let kind = PseudoKind::from_progid(progid)?;
        let object = PseudoObject {
            kind,
            dict: BTreeMap::new(),
        };
        if let Some(i) = self.pseudo_objects.iter().position(|slot| slot.is_none()) {
            self.pseudo_objects[i] = Some(object);
            return Some(i + 1);
        }
        self.pseudo_objects.push(Some(object));
        Some(self.pseudo_objects.len())
    }

    fn pseudo_object(&mut self, handle: usize) -> Option<&mut PseudoObject> {
        self.pseudo_objects
            .get_mut(handle.checked_sub(1)?)?
            .as_mut()
    }

    fn registry_data_to_value(data: &RegistryData) -> Value {
        match data {
            RegistryData::Sz(t) | RegistryData::ExpandSz(t) => Value::Str(t.clone()),
            RegistryData::MultiSz(items) => {
                Value::array(items.iter().map(|i| Value::Str(i.clone())).collect())
            }
            RegistryData::Dword(v) => Value::Int(*v as i64),
            RegistryData::Qword(v) => Value::Int(*v as i64),
            RegistryData::Binary(b) => Value::Binary(Rc::new(b.clone())),
        }
    }

    /// Property read on a pseudo object (`$o.Count`).
    fn pseudo_com_get(&mut self, handle: usize, member: &str) -> Option<Result<Value, String>> {
        let kind = self
            .pseudo_objects
            .get(handle.checked_sub(1)?)?
            .as_ref()?
            .kind;
        match kind {
            PseudoKind::Dictionary => {
                if member.eq_ignore_ascii_case("count") {
                    let n = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| o.dict.len())
                        .unwrap_or(0);
                    Some(Ok(Value::Int(n as i64)))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Method call on a pseudo object.
    fn pseudo_com_call(
        &mut self,
        handle: usize,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Option<Result<Value, String>> {
        let kind = self
            .pseudo_objects
            .get(handle.checked_sub(1)?)?
            .as_ref()?
            .kind;
        let arg_str_at = |args: &[Value], i: usize| -> String {
            args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
        };
        match kind {
            PseudoKind::Dictionary => {
                let m = member.to_ascii_lowercase();
                if m == "add" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let value = args.get(1).cloned().unwrap_or(Value::Null);
                    let object = self.pseudo_object(handle)?;
                    if object.dict.contains_key(&key) {
                        return Some(Err("This key is already associated with an element of this collection".into()));
                    }
                    object.dict.insert(key, value);
                    return Some(Ok(Value::Null));
                }
                if m == "exists" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let hit = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| o.dict.contains_key(&key))
                        .unwrap_or(false);
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "item" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    let value = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .and_then(|o| o.dict.get(&key).cloned())
                        .unwrap_or(Value::Null);
                    return Some(Ok(value));
                }
                if m == "remove" {
                    let key = arg_str_at(args, 0).to_ascii_lowercase();
                    self.pseudo_object(handle)?.dict.remove(&key);
                    return Some(Ok(Value::Null));
                }
                if m == "removeall" {
                    self.pseudo_object(handle)?.dict.clear();
                    return Some(Ok(Value::Null));
                }
                if m == "keys" || m == "items" {
                    let list = self
                        .pseudo_objects
                        .get(handle - 1)?
                        .as_ref()
                        .map(|o| match m.as_str() {
                            "keys" => o.dict.keys().map(|k| Value::Str(k.clone())).collect(),
                            _ => o.dict.values().cloned().collect(),
                        })
                        .unwrap_or_default();
                    return Some(Ok(Value::array(list)));
                }
                None
            }
            PseudoKind::WScriptShell => {
                let m = member.to_ascii_lowercase();
                // WSH registry names carry the value in the path itself:
                // `HKCU\...\Value` addresses a value, a trailing `\` the
                // `(Default)` value of the key.
                let split_name = |full: &str| -> (String, String) {
                    match full.rfind('\\') {
                        Some(i) => (full[..i].to_string(), full[i + 1..].to_string()),
                        None => (full.to_string(), String::new()),
                    }
                };
                if m == "regread" {
                    let (key, value) = split_name(&arg_str_at(args, 0));
                    let data = self.registry.read(&key, &value)?;
                    return Some(Ok(Self::registry_data_to_value(&data)));
                }
                if m == "regwrite" {
                    if !ctx.effect_allowed(EffectKind::RegistryWrite) {
                        return Some(Err("@error (writes denied)".into()));
                    }
                    // RegWrite(Name, Value [, Type]).
                    let (key, value_name) = split_name(&arg_str_at(args, 0));
                    let value = args.get(1).cloned().unwrap_or(Value::Null);
                    let data = RegistryData::from_autoit(
                        args.get(2).and_then(reg_type_code),
                        &value,
                    )?;
                    let ok = self.registry.write(&key, &value_name, data);
                    return Some(Ok(Value::Bool(ok)));
                }
                if m == "regdelete" {
                    if !ctx.effect_allowed(EffectKind::RegistryWrite) {
                        return Some(Err("@error (writes denied)".into()));
                    }
                    let path = arg_str_at(args, 0);
                    let ok = if let Some(key) = path.strip_suffix('\\') {
                        self.registry.delete_key(key, true)
                    } else {
                        let (key, value) = split_name(&path);
                        self.registry.delete_value(&key, &value)
                    };
                    return Some(Ok(Value::Bool(ok)));
                }
                if m == "expandenvironmentstrings" {
                    let raw = arg_str_at(args, 0);
                    // Expand %VAR% against the *host* environment: reads only.
                    let mut out = String::new();
                    let mut rest = raw.as_str();
                    while let Some(start) = rest.find('%') {
                        out.push_str(&rest[..start]);
                        let tail = &rest[start + 1..];
                        match tail.find('%') {
                            Some(end) => {
                                let name = &tail[..end];
                                out.push_str(
                                    &host_env(name).unwrap_or_else(|| format!("%{name}%")),
                                );
                                rest = &tail[end + 1..];
                            }
                            None => {
                                out.push_str(tail);
                                break;
                            }
                        }
                    }
                    return Some(Ok(Value::Str(out)));
                }
                if m == "run" || m == "runwait" {
                    if !ctx.effect_allowed(EffectKind::Spawn) {
                        return Some(Err("@error (spawn denied)".into()));
                    }
                    let cmdline = arg_str_at(args, 0);
                    let mut tokens = cmdline.splitn(2, ' ');
                    let program = tokens.next().unwrap_or_default();
                    let params = tokens.next().unwrap_or_default();
                    match shell::spawn(program, params, "", 0) {
                        Ok(mut child) => {
                            let v = if m == "runwait" {
                                let _ = child.wait();
                                Value::Int(0)
                            } else {
                                Value::Int(child.id() as i64)
                            };
                            return Some(Ok(v));
                        }
                        Err(_) => return Some(Err("@error (spawn failed)".into())),
                    }
                }
                None
            }
            PseudoKind::FileSystemObject => {
                let m = member.to_ascii_lowercase();
                if m == "fileexists" {
                    let path = arg_str_at(args, 0);
                    let hit = Path::new(&path).exists()
                        || self
                            .sandbox_files
                            .contains_key(&normalise_sandbox_path(&path));
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "driveexists" {
                    let path = arg_str_at(args, 0);
                    let hit = self.find_drive(&path).is_some()
                        || Path::new(&format!("{}\\", path.trim())).exists();
                    return Some(Ok(Value::Bool(hit)));
                }
                if m == "getspecialfolder" {
                    let which = args.first().map(|v| v.to_int()).unwrap_or(0);
                    let dir = match which {
                        1 => self.paths.system_dir.clone(),
                        2 => self.paths.temp(),
                        _ => self.paths.windows_dir.clone(),
                    };
                    return Some(Ok(Value::Str(dir)));
                }
                if m == "gettempname" {
                    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Some(Ok(Value::Str(format!("au3{}.tmp", std::process::id() as usize + n))));
                }
                // Pure path arithmetic: GetExtensionName / GetBaseName /
                // GetFileName / GetParentFolderName.
                let path = arg_str_at(args, 0);
                let file_name = path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&path)
                    .to_string();
                let value = match m.as_str() {
                    "getfilename" => Some(file_name.clone()),
                    "getextensionname" => file_name
                        .rsplit_once('.')
                        .map(|(_, ext)| ext.to_string()),
                    "getbasename" => Some(
                        file_name
                            .rsplit_once('.')
                            .map(|(base, _)| base.to_string())
                            .unwrap_or_else(|| file_name.clone()),
                    ),
                    "getparentfoldername" => Some(
                        match path.rfind(['\\', '/']) {
                            Some(i) => path[..i].to_string(),
                            None => String::new(),
                        }
                    ),
                    _ => None,
                }?;
                Some(Ok(Value::Str(value)))
            }
        }
    }

    /// Drain the callback invocations the enumerators scheduled.
    pub fn take_pending_callbacks(&mut self) -> Vec<(String, Vec<Value>)> {
        std::mem::take(&mut self.pending_callbacks)
    }

    /// Hand out a `DllOpen` handle for `name`.
    fn open_dll(&mut self, name: &str) -> i64 {
        if let Some(i) = self.dlls.iter().position(|slot| slot.is_none()) {
            self.dlls[i] = Some(name.to_string());
            return i as i64 + 1;
        }
        self.dlls.push(Some(name.to_string()));
        self.dlls.len() as i64
    }

    fn resource(&self, handle: i64) -> Option<&ResourceHandle> {
        let index = usize::try_from(handle).ok()?.checked_sub(1)?;
        self.handles.get(index)?.as_ref()
    }

    /// Materialise a locked resource and return its address.
    fn lock_resource(&mut self, handle: i64) -> Option<Value> {
        let index = usize::try_from(handle).ok()?.checked_sub(1)?;
        if let Some(addr) = self.handles.get(index)?.as_ref()?.address {
            return Some(Value::Int(addr as i64));
        }
        let data = self.handles.get(index)?.as_ref()?.data.clone();
        let addr = self.allocate(data.len());
        self.blobs.push((addr, Rc::new(RefCell::new(data))));
        if let Some(slot) = self.handles.get_mut(index).and_then(|h| h.as_mut()) {
            slot.address = Some(addr);
        }
        Some(Value::Int(addr as i64))
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
        // The tail is `type, value, type, value, ...`.
        let pairs: Vec<(String, Value)> = args
            .get(3..)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].to_autoit_string(), pair[1].clone()))
            .collect();

        let outcome = self.dll_call_inner(&function, &pairs).or_else(|| {
            // AutoIt resolves an unsuffixed name to its ANSI variant
            // (`MessageBox` → `MessageBoxA`), so an arm written for `...A`
            // must also answer the bare name.
            let lower = function.trim().to_ascii_lowercase();
            if lower.is_empty() || lower.ends_with('a') || lower.ends_with('w') {
                return None;
            }
            self.dll_call_inner(&format!("{lower}A"), &pairs)
        });

        match outcome {
            Some(out) => {
                ctx.set_error(0, 0);
                // `[return value, arg1, arg2, ...]`, as AutoIt hands it back.
                let mut result = Vec::with_capacity(pairs.len() + 1);
                result.push(out.retval);
                for (i, (_, value)) in pairs.iter().enumerate() {
                    let updated = out
                        .writes
                        .iter()
                        .find(|(index, _)| *index == i)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| value.clone());
                    result.push(updated);
                }
                Value::array(result)
            }
            // Not emulated: hand control back to the script's error handling
            // rather than inventing a result. AutoIt returns 0 (not an array)
            // when a call fails, and scripts test `@error` first.
            None => {
                if self.trace_dll && self.traced.insert(function.to_ascii_lowercase()) {
                    eprintln!(
                        "[winemu] DllCall not emulated: {}!{}",
                        arg_str(args, 0),
                        function
                    );
                }
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    /// Dispatch one emulated `DllCall`.
    fn dll_call_inner(
        &mut self,
        function: &str,
        pairs: &[(String, Value)],
    ) -> Option<DllOutcome> {
        let arg = |i: usize| pairs.get(i).map(|(_, v)| v.clone());
        let values: Vec<Value> = pairs.iter().map(|(_, v)| v.clone()).collect();
        // Arms that branch on A/W suffixes compare against the lower-cased
        // name, not the caller's spelling.
        let lower = function.to_ascii_lowercase();
        let wide_name = lower.ends_with('w');
        match lower.as_str() {
            "getversionexw" | "getversionexa" | "rtlgetversion" => {
                Some(DllOutcome::value(self.fill_version_struct(&values)?))
            }
            "getversion" => Some(DllOutcome::value(Value::Int(
                self.version.packed_get_version() as i64,
            ))),
            "getsysteminfo" | "getnativesysteminfo" => {
                Some(DllOutcome::value(self.fill_system_info(&values)?))
            }
            "getlasterror" | "setlasterror" => Some(DllOutcome::value(Value::Int(0))),
            "getcurrentprocessid" | "getcurrentthreadid" => {
                Some(DllOutcome::value(Value::Int(std::process::id() as i64)))
            }
            "getcurrentprocess" => Some(DllOutcome::value(Value::Int(-1))),
            "gettickcount" | "gettickcount64" => Some(DllOutcome::value(Value::Int(
                self.origin.elapsed().as_millis() as i64,
            ))),
            // "Is this pointer bad?" — our addresses only ever name buffers the
            // emulation allocated, so the honest answer is "no".
            "isbadreadptr" | "isbadwriteptr" => Some(DllOutcome::value(Value::Int(0))),

            // ---------------- module resources ----------------
            "getmodulehandlew" | "getmodulehandlea" => {
                // Either source can answer `FindResourceW`: the image, or
                // resources already extracted next to the script. Failing
                // here when neither exists keeps the boundary visible.
                if self.module.is_none() && self.resource_dirs.is_empty() {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(EMULATED_IMAGE_BASE)))
            }
            "findresourcew" | "findresourcea" => {
                let name = resource_selector(arg(1).as_ref())?;
                let kind = resource_selector(arg(2).as_ref())?;
                // Resources extracted to files win over the image: an analysis
                // usually has the payload directory and not the `.exe` it came
                // from, and looking in the working directory first is what
                // makes that work.
                let data = match PeImage::find_resource_file(&self.resource_dirs, &name) {
                    Some(bytes) => {
                        if self.trace_dll {
                            eprintln!("[winemu] resource {} from file", name.name.as_deref().unwrap_or("?"));
                        }
                        bytes
                    }
                    None => self.module.as_ref()?.find(&name, &kind)?.data.clone(),
                };
                self.handles.push(Some(ResourceHandle {
                    data,
                    address: None,
                }));
                Some(DllOutcome::value(Value::Int(self.handles.len() as i64)))
            }
            "sizeofresource" => {
                let handle = arg(1).map(|v| v.to_int()).unwrap_or(0);
                let size = self.resource(handle)?.data.len() as i64;
                Some(DllOutcome::value(Value::Int(size)))
            }
            "loadresource" => {
                let handle = arg(1).map(|v| v.to_int()).unwrap_or(0);
                self.resource(handle)?;
                Some(DllOutcome::value(Value::Int(handle)))
            }
            "lockresource" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                Some(DllOutcome::value(self.lock_resource(handle)?))
            }
            "rtlmovememory" | "copymemory" => {
                let dest = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let src = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let len = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let bytes = self.memory_read(src, len)?;
                if !self.memory_write(dest, &bytes) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(0)))
            }

            // ---------------- modules ----------------
            "loadlibraryw" | "loadlibrarya" => {
                let name = self.c_string_arg(arg(0)?)?;
                Some(DllOutcome::value(Value::Int(self.open_dll(&name))))
            }
            "getprocaddress" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                if self.dll_name(handle).is_none() {
                    return None;
                }
                // A non-zero pseudo address: enough for a script to detect
                // "the export exists" and to compare two exports.
                Some(DllOutcome::value(Value::Int(self.allocate(0x10) as i64)))
            }
            "getmodulefilenamew" | "getmodulefilenamea" => {
                let wide = wide_name;
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let name = self
                    .dll_name(handle)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "emulated.dll".to_string());
                let full = format!("{}\\{}", self.paths.windows_dir, name);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let size = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                if size == 0 || !self.write_c_string(buf, &full, wide, size) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(full.len() as i64)))
            }

            // ---------------- memory ----------------
            "virtualalloc" | "virtualallocex" | "heapalloc" => {
                let size = arg(1).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                if size == 0 {
                    return None;
                }
                let addr = self.allocate(size);
                self.blobs
                    .push((addr, Rc::new(RefCell::new(vec![0u8; size]))));
                Some(DllOutcome::value(Value::Int(addr as i64)))
            }
            "virtualfree" | "heapfree" => Some(DllOutcome::value(Value::Bool(true))),
            "getprocessheap" => Some(DllOutcome::value(Value::Int(0x1))),

            // ---------------- sandboxed files ----------------
            "createfilew" | "createfilea" => {
                let path = self.c_string_arg(arg(0)?)?;
                let access = arg(1).map(|v| v.to_int()).unwrap_or(0);
                let write = access & 0x4000_0000 != 0; // GENERIC_WRITE
                let read = access & 0x8000_0000 != 0 || !write; // GENERIC_READ
                if !read && !write {
                    return None;
                }
                let handle = self.next_file_handle;
                self.next_file_handle += 1;
                let content = if write {
                    Vec::new()
                } else {
                    match self.sandbox_files.get(&normalise_sandbox_path(&path)) {
                        Some(c) => c.clone(),
                        None => return None, // file not found
                    }
                };
                self.open_files
                    .insert(handle, OpenFile { path, content, write, pos: 0 });
                Some(DllOutcome::value(Value::Int(handle)))
            }
            "readfile" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let count = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let lpread = arg(3).map(|v| v.to_int()).unwrap_or(0) as u64;
                let file = self.open_files.get_mut(&handle)?;
                if file.write {
                    return None;
                }
                let start = (file.pos as usize).min(file.content.len());
                let end = (start + count).min(file.content.len());
                let bytes = file.content[start..end].to_vec();
                file.pos = end as u64;
                if !self.memory_write(buf, &bytes) {
                    return None;
                }
                if lpread != 0 {
                    self.memory_write(lpread, &(bytes.len() as u32).to_le_bytes());
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "writefile" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let buf = arg(1).map(|v| v.to_int()).unwrap_or(0) as u64;
                let count = arg(2).map(|v| v.to_int()).unwrap_or(0).max(0) as usize;
                let lpwritten = arg(3).map(|v| v.to_int()).unwrap_or(0) as u64;
                let bytes = self.memory_read(buf, count)?;
                let file = self.open_files.get_mut(&handle)?;
                if !file.write {
                    return None;
                }
                file.content.extend_from_slice(&bytes);
                if lpwritten != 0 {
                    self.memory_write(lpwritten, &(bytes.len() as u32).to_le_bytes());
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "getfilesize" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let len = self
                    .open_files
                    .get(&handle)
                    .map(|f| f.content.len() as i64)
                    .unwrap_or(-1);
                if len < 0 {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(len)))
            }
            "closehandle" => {
                let handle = arg(0).map(|v| v.to_int()).unwrap_or(0);
                match self.open_files.remove(&handle) {
                    Some(f) => {
                        if f.write {
                            self.sandbox_files
                                .insert(normalise_sandbox_path(&f.path), f.content);
                        }
                        Some(DllOutcome::value(Value::Bool(true)))
                    }
                    None => Some(DllOutcome::value(Value::Bool(true))),
                }
            }

            // ---------------- CRT-style strings ----------------
            "lstrlenw" | "lstrlena" => {
                let s = self.c_string_arg(arg(0)?)?;
                Some(DllOutcome::value(Value::Int(s.chars().count() as i64)))
            }
            "lstrcpyw" | "lstrcpya" => {
                let dst = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let src = self.c_string_arg(arg(1)?)?;
                let wide = wide_name;
                let max = if wide { (src.chars().count() + 1) * 2 } else { src.len() + 1 };
                if !self.write_c_string(dst, &src, wide, max) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(dst as i64)))
            }
            "lstrcatw" | "lstrcata" => {
                let dst = arg(0).map(|v| v.to_int()).unwrap_or(0) as u64;
                let tail = self.c_string_arg(arg(1)?)?;
                let wide = wide_name;
                let mut head = self.c_string_at(dst, wide)?;
                head.push_str(&tail);
                let max = if wide { (head.chars().count() + 1) * 2 } else { head.len() + 1 };
                if !self.write_c_string(dst, &head, wide, max) {
                    return None;
                }
                Some(DllOutcome::value(Value::Int(dst as i64)))
            }

            // ---------------- scripted enumeration ----------------
            "enumwindows" | "enumchildwindows" | "enumthreadwindows" => {
                let cb = arg(0).map(|v| v.to_int()).unwrap_or(0);
                let lparam = arg(function.starts_with("enumthread") as usize + 1)
                    .map(|v| v.to_int())
                    .unwrap_or(0);
                let index = callback_index(cb)?;
                let name = self
                    .callbacks
                    .get(index)
                    .cloned()
                    .flatten()?;
                for hwnd in self.scripted_windows.clone() {
                    self.pending_callbacks
                        .push((name.clone(), vec![Value::Int(hwnd), Value::Int(lparam)]));
                }
                Some(DllOutcome::value(Value::Bool(true)))
            }

            // ---------------- CryptoAPI ----------------
            "cryptacquirecontexta" | "cryptacquirecontextw" => {
                Some(DllOutcome::with(
                    Value::Bool(true),
                    0,
                    Value::Int(0x0c00_0001),
                ))
            }
            "cryptreleasecontext" => Some(DllOutcome::value(Value::Bool(true))),
            "cryptcreatehash" => self.crypt_create_hash(&arg(1)?),
            "crypthashdata" => self.crypt_hash_data(&arg(0)?, &arg(1)?, &arg(2)?),
            "cryptgethashparam" => self.crypt_get_hash_param(&arg(0)?, &arg(1)?, &arg(2)?),
            "cryptderivekey" => self.crypt_derive_key(&arg(1)?, &arg(2)?),
            "cryptdecrypt" => {
                let final_block = arg(2).map(|v| v.is_truthy()).unwrap_or(false);
                self.crypt_decrypt(&arg(0)?, &arg(4)?, &arg(5)?, final_block)
            }
            // KP_IV = 7: the IV the derived key already carries.
            "cryptgetkeyparam" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                let key = self.crypto.keys.get(index)?.as_ref()?.clone();
                match arg(1)?.to_int() {
                    7 => {
                        self.write_buffer(&arg(2)?, &key.iv);
                        Some(DllOutcome::with(
                            Value::Bool(true),
                            2,
                            Value::Binary(std::rc::Rc::new(key.iv)),
                        ))
                    }
                    _ => None,
                }
            }
            "cryptsetkeyparam" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                let value = self.read_buffer(&arg(2)?, 0)?;
                if arg(1)?.to_int() == 7 {
                    if let Some(slot) = self.crypto.keys.get_mut(index).and_then(|k| k.as_mut()) {
                        slot.iv = value;
                    }
                    Some(DllOutcome::value(Value::Bool(true)))
                } else {
                    None
                }
            }
            "cryptdestroyhash" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                *self.crypto.hashes.get_mut(index)? = None;
                Some(DllOutcome::value(Value::Bool(true)))
            }
            "cryptdestroykey" => {
                let index = usize::try_from(arg(0)?.to_int()).ok()?.checked_sub(1)?;
                *self.crypto.keys.get_mut(index)? = None;
                Some(DllOutcome::value(Value::Bool(true)))
            }

            // ---------------- LZNT1 ----------------
            "rtlgetcompressionworkspacesize" => {
                Some(DllOutcome::with(Value::Int(0), 1, Value::Int(0)))
            }
            "rtldecompressbuffer" => {
                self.rtl_decompress_buffer(&arg(0)?, &arg(1)?, &arg(2)?, &arg(3)?, &arg(4)?)
            }
            _ => None,
        }
    }

    // ----- CryptoAPI -----

    /// `CryptCreateHash(hProv, algid, hKey, flags, phHash)`.
    fn crypt_create_hash(&mut self, algid: &Value) -> Option<DllOutcome> {
        let Some(alg) = crypto::HashAlg::from_algid(algid.to_int() as u32) else {
            if self.trace_dll {
                eprintln!("[winemu] CryptCreateHash: unsupported hash algid {:#x}", algid.to_int());
            }
            return None;
        };
        let slot = Some(HashObject {
            alg,
            data: Vec::new(),
        });
        let handle = if let Some(i) = self.crypto.hashes.iter().position(|h| h.is_none()) {
            self.crypto.hashes[i] = slot;
            i as i64 + 1
        } else {
            self.crypto.hashes.push(slot);
            self.crypto.hashes.len() as i64
        };
        // The sample hands `phHash` a literal 0 and reads the handle out of
        // `$result[5]`, so the array slot is the only channel that matters.
        Some(DllOutcome::with(Value::Bool(true), 4, Value::Int(handle)))
    }

    /// `CryptHashData(hHash, pbData, dwDataLen, flags)`.
    fn crypt_hash_data(
        &mut self,
        handle: &Value,
        buffer: &Value,
        len: &Value,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let len = len.to_int().max(0) as usize;
        let bytes = self.read_buffer(buffer, len)?;
        let slot = self.crypto.hashes.get_mut(index)?.as_mut()?;
        slot.data.extend_from_slice(&bytes);
        Some(DllOutcome::value(Value::Bool(true)))
    }

    /// `CryptGetHashParam(hHash, param, pbData, pdwDataLen, flags)`.
    fn crypt_get_hash_param(
        &mut self,
        handle: &Value,
        param: &Value,
        buffer: &Value,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let alg = self.crypto.hashes.get(index)?.as_ref()?.alg;
        let data = self.crypto.hashes.get(index)?.as_ref()?.data.clone();
        match param.to_int() {
            // HP_HASHSIZE
            4 => {
                let size = Value::Int(alg.digest_len() as i64);
                self.write_buffer(buffer, &(alg.digest_len() as u32).to_le_bytes());
                Some(DllOutcome::with(Value::Bool(true), 2, size))
            }
            // HP_HASHVAL
            2 => {
                let digest = alg.digest(&data);
                self.write_buffer(buffer, &digest);
                Some(DllOutcome::with(
                    Value::Bool(true),
                    2,
                    Value::Binary(std::rc::Rc::new(digest)),
                ))
            }
            // HP_ALGID
            1 => Some(DllOutcome::with(Value::Bool(true), 2, Value::Int(0))),
            _ => None,
        }
    }

    /// `CryptDeriveKey(hProv, algid, hBaseData, flags, phKey)`.
    fn crypt_derive_key(&mut self, algid: &Value, base: &Value) -> Option<DllOutcome> {
        let Some(alg) = crypto::CipherAlg::from_algid(algid.to_int() as u32) else {
            if self.trace_dll {
                eprintln!("[winemu] CryptDeriveKey: unsupported cipher algid {:#x}", algid.to_int());
            }
            return None;
        };
        // The key material is the digest of the hash object handed in, run
        // through CryptoAPI's derivation for the requested algorithm.
        let index = usize::try_from(base.to_int()).ok()?.checked_sub(1)?;
        let (hash_alg, digest) = {
            let hash = self.crypto.hashes.get(index)?.as_ref()?;
            (hash.alg, hash.alg.digest(&hash.data))
        };
        let (key, iv) = alg.derive_key(hash_alg, &digest);
        let handle = if let Some(i) = self.crypto.keys.iter().position(|k| k.is_none()) {
            self.crypto.keys[i] = Some(KeyObject { alg, key, iv });
            i as i64 + 1
        } else {
            self.crypto.keys.push(Some(KeyObject { alg, key, iv }));
            self.crypto.keys.len() as i64
        };
        Some(DllOutcome::with(Value::Bool(true), 4, Value::Int(handle)))
    }

    /// `CryptDecrypt(hKey, hHash, final, flags, pbData, pdwDataLen)`.
    fn crypt_decrypt(
        &mut self,
        handle: &Value,
        buffer: &Value,
        len: &Value,
        final_block: bool,
    ) -> Option<DllOutcome> {
        let index = usize::try_from(handle.to_int()).ok()?.checked_sub(1)?;
        let key = self.crypto.keys.get(index)?.as_ref()?.clone();
        let len = len.to_int().max(0) as usize;
        let data = self.read_buffer(buffer, len)?;
        let mut plain = key.alg.apply(&key.key, &key.iv, &data);
        // CryptoAPI's block ciphers pad to the block size with PKCS#7, and the
        // final `CryptDecrypt` strips it — leaving it in would append up to a
        // block of 0x10 bytes to the plaintext.
        if final_block {
            if let Some(keep) = pkcs7_kept_len(&plain) {
                plain.truncate(keep);
            }
        }
        if !self.write_buffer(buffer, &plain) {
            return None;
        }
        Some(DllOutcome::with(
            Value::Bool(true),
            5,
            Value::Int(plain.len() as i64),
        ))
    }

    /// `RtlDecompressBuffer(format, outBuf, outLen, inBuf, inLen, pOutLen)`.
    fn rtl_decompress_buffer(
        &mut self,
        format: &Value,
        out_buf: &Value,
        out_len: &Value,
        in_buf: &Value,
        in_len: &Value,
    ) -> Option<DllOutcome> {
        if format.to_int() as u32 != compress::COMPRESSION_FORMAT_LZNT1 {
            return None;
        }
        let input = self.read_buffer(in_buf, in_len.to_int().max(0) as usize)?;
        let mut plain = compress::decompress(&input)?;
        let capacity = out_len.to_int().max(0) as usize;
        if capacity > 0 {
            plain.truncate(capacity);
        }
        if !self.write_buffer(out_buf, &plain) {
            return None;
        }
        Some(DllOutcome::with(
            Value::Int(0),
            5,
            Value::Int(plain.len() as i64),
        ))
    }

    // ----- buffers -----

    /// Read up to `len` bytes from whatever a `DllCall` argument names: a
    /// `DllStruct` handle (AutoIt's `struct*`), an emulated address, a binary
    /// value, or a string.
    fn read_buffer(&self, value: &Value, len: usize) -> Option<Vec<u8>> {
        match value {
            Value::Binary(bytes) => {
                let mut out = bytes.as_ref().clone();
                out.truncate(if len == 0 { out.len() } else { len });
                Some(out)
            }
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            _ => {
                let handle = value.to_int();
                let bytes = self.struct_ref(handle).map(|s| s.bytes())?;
                let take = if len == 0 { bytes.len() } else { len.min(bytes.len()) };
                Some(bytes[..take].to_vec())
            }
        }
    }

    /// Write `bytes` into whatever a `DllCall` argument names.
    fn write_buffer(&mut self, value: &Value, bytes: &[u8]) -> bool {
        let handle = value.to_int();
        if let Some(s) = self.struct_any_mut(handle) {
            let n = bytes.len().min(s.size());
            return s.write_all(&bytes[..n]);
        }
        self.memory_write(handle as u64, bytes)
    }

    /// Write `OSVERSIONINFO(W/EX)` fields into the struct argument.
    fn fill_version_struct(&mut self, extra: &[Value]) -> Option<Value> {
        let handle = struct_handle_arg(extra)?;
        let version = self.version;
        let s = self.struct_any_mut(handle)?;
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
        let s = self.struct_any_mut(handle)?;
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
        if !ctx.effect_allowed(EffectKind::RegistryWrite) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let key = arg_str(args, 0);
        let value = arg_str(args, 1);
        // `RegWrite(key, value, type, data)`; a 3-argument call omits the type.
        let (type_code, data) = if args.len() >= 4 {
            (reg_type_code(&args[2]), args[3].clone())
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
        if !ctx.effect_allowed(EffectKind::RegistryWrite) {
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
        // Cache by (path, mtime): a script polling ClipGet in a loop must not
        // pay a file read plus a `current_dir` syscall per iteration, while an
        // external edit of the backing file is still picked up.
        let path = self.clipboard_path();
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        {
            let cache = self.clipboard_cache.borrow();
            if let Some((cp, cm, ctext)) = cache.as_ref() {
                if cp == &path && cm == &mtime {
                    ctx.set_error(0, 0);
                    return Value::Str(ctext.clone());
                }
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                *self.clipboard_cache.borrow_mut() = Some((path, mtime, text.clone()));
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
        if !ctx.effect_allowed(EffectKind::FileWrite) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let text = arg_str(args, 0);
        let path = self.clipboard_path();
        let ok = std::fs::write(&path, text.clone()).is_ok();
        if ok {
            // Write through: the cache matches what is on disk.
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            *self.clipboard_cache.borrow_mut() = Some((path, mtime, text));
        } else {
            self.clipboard_cache.borrow_mut().take();
        }
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

    /// Move a file or directory into the emulated recycle directory.
    fn recycle(&self, path: &str) -> bool {
        let src = std::path::Path::new(path);
        if !src.exists() {
            return false;
        }
        if std::fs::create_dir_all(&self.recycle_dir).is_err() {
            return false;
        }
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item".to_string());
        let stamp = self.origin.elapsed().as_millis();
        std::fs::rename(src, self.recycle_dir.join(format!("{stamp}_{name}"))).is_ok()
    }

    /// Serve `FileInstall`: copy `source` to `dest`, falling back to the loaded
    /// module's `RT_RCDATA` resources when the source is not on disk.
    fn file_install(&self, source: &str, dest: &str, no_overwrite: bool) -> bool {
        if dest.is_empty() {
            return false;
        }
        if no_overwrite && std::path::Path::new(dest).exists() {
            return true;
        }
        let src = std::path::Path::new(source);
        if src.is_file() {
            return std::fs::copy(src, dest).is_ok();
        }
        let Some(module) = &self.module else {
            return false;
        };
        let basename = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if basename.is_empty() {
            return false;
        }
        for name in [basename.clone(), format!("__{basename}")] {
            if let Some(res) = module.find(&Selector::name(name), &Selector::id(10)) {
                return std::fs::write(dest, &res.data).is_ok();
            }
        }
        false
    }

    /// The first unused drive letter from `D:` to `Z:`.
    fn free_drive_letter(&self) -> Option<char> {
        ('D'..='Z').find(|letter| {
            !self.drive_maps.iter().any(|(d, _)| d.starts_with(*letter))
                && !self.drives.iter().any(|d| d.letter == *letter)
        })
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

    fn take_pending_callbacks(&mut self) -> Vec<(String, Vec<Value>)> {
        self.take_pending_callbacks()
    }

    // ----- pseudo COM -----

    fn obj_get(
        &mut self,
        obj: &ObjRef,
        member: &str,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        match self.pseudo_com_get(obj.handle, member) {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(what)) => Err(RuntimeError::Unsupported { what, span: None }),
            // AutoIt does not distinguish properties from methods: a bare
            // `$d.Keys` is a real call. Fall through to a no-arg invocation.
            None => self.obj_call(obj, member, &[], ctx),
        }
    }

    fn obj_call(
        &mut self,
        obj: &ObjRef,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        match self.pseudo_com_call(obj.handle, member, args, ctx) {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(what)) => Err(RuntimeError::Unsupported { what, span: None }),
            None => Ok(None),
        }
    }

    fn provides(&self, name: &str) -> bool {
        self.enabled
            && (FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
                || gui::GuiState::provides(name))
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
        // The GUI half is a self-contained state machine; let it answer first.
        if let Some(value) = self.gui.call(&key, &args, ctx) {
            return Ok(Some(value));
        }
        let value = match key.as_str() {
            // ---------------- DllStruct ----------------
            "dllstructcreate" => {
                let definition = arg_str(&args, 0);
                // `DllStructCreate($def, $ptr)` maps the struct over memory
                // that already exists, so writes through either view are seen
                // by the other — that is how a script reads a decrypted buffer
                // back out of the struct it passed to a `DllCall`.
                let pointer = args.get(1).map(|v| v.to_int()).unwrap_or(0);
                let over = if pointer > 0 {
                    self.memory_storage(pointer as u64)
                } else {
                    None
                };
                let created = match over {
                    Some((storage, offset, address)) => DllStruct::create_over(
                        &definition,
                        self.arch,
                        storage,
                        offset,
                        address,
                    ),
                    None => DllStruct::create(&definition, self.arch),
                };
                match created {
                    Ok(mut s) => {
                        // Give it an address so `DllStructGetPtr` hands out
                        // something `RtlMoveMemory` can write through.
                        if s.address() == 0 {
                            let size = s.size();
                            let address = self.allocate(size);
                            s.set_address(address);
                        }
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
                let address = self.struct_ref(handle).map(|s| s.address()).unwrap_or(0);
                if address != 0 {
                    ctx.set_error(0, 0);
                    Value::Int(address as i64)
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
            // `DllOpen` just hands back a handle that is passed to `DllCall` in
            // place of the file name; our `DllCall` never loads anything, so a
            // counter is enough. `DllClose` forgets it again.
            "dllopen" => {
                let name = arg_str(&args, 0);
                let handle = self.open_dll(&name);
                ctx.set_error(0, 0);
                Value::Int(handle)
            }
            "dllclose" => {
                let handle = arg_int(&args, 0);
                let ok = handle >= 1 && (handle as usize) <= self.dlls.len();
                if ok {
                    self.dlls[handle as usize - 1] = None;
                    ctx.set_error(0, 0);
                    Value::Int(1)
                } else {
                    ctx.set_error(1, 0);
                    Value::Int(0)
                }
            }
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

            // ---------------- Windows files / PE resources ----------------
            "filegetversion" => {
                let path = arg_str(&args, 0);
                let field = arg_str(&args, 1);
                let field = field.trim();
                let field = if field.is_empty() {
                    "FileVersion"
                } else {
                    field
                };
                match crate::winfmt::verinfo::read(&path) {
                    Some(info) => {
                        if let Some(v) = info.string(field) {
                            ctx.set_error(0, 0);
                            Value::Str(v.to_string())
                        } else if field.eq_ignore_ascii_case("FileVersion") && info.dotted().is_some()
                        {
                            ctx.set_error(0, 0);
                            Value::Str(info.dotted().unwrap_or_default())
                        } else {
                            ctx.set_error(1, 0);
                            Value::str("")
                        }
                    }
                    // No version resource (or unreadable file): AutoIt's
                    // documented failure value for a missing version.
                    None => {
                        ctx.set_error(1, 0);
                        Value::Str("0.0.0.0".to_string())
                    }
                }
            }
            "filecreateshortcut" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let target = arg_str(&args, 0);
                let lnk = arg_str(&args, 1);
                if target.is_empty() || lnk.is_empty() {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let sc = crate::winfmt::shortcut::Shortcut {
                    target,
                    working_dir: arg_str(&args, 2),
                    arguments: arg_str(&args, 3),
                    description: arg_str(&args, 4),
                    icon_location: arg_str(&args, 5),
                    hotkey: parse_hotkey(&arg_str(&args, 6)),
                    icon_index: args.get(7).map(|v| v.to_int() as i32).unwrap_or(0),
                    show_command: args.get(8).map(|v| v.to_int() as u32).unwrap_or(1),
                };
                let ok = crate::winfmt::shortcut::write(&lnk, &sc).is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filegetshortcut" => {
                let lnk = arg_str(&args, 0);
                match crate::winfmt::shortcut::read(&lnk) {
                    Some(sc) => {
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Str(sc.target),
                            Value::Str(sc.working_dir),
                            Value::Str(sc.arguments),
                            Value::Str(sc.description),
                            Value::Str(sc.icon_location),
                            Value::Int(i64::from(sc.icon_index)),
                            Value::Int(i64::from(sc.show_command)),
                        ])
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::array(vec![Value::Int(0)])
                    }
                }
            }
            "filecreatentfslink" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let link = arg_str(&args, 0);
                let target = arg_str(&args, 1);
                let ok = if arg_int(&args, 2) == 1 {
                    create_junction(&target, &link)
                } else {
                    std::fs::hard_link(&target, &link).is_ok()
                };
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filerecycle" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let ok = self.recycle(&arg_str(&args, 0));
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filerecycleempty" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let ok = match std::fs::remove_dir_all(&self.recycle_dir) {
                    Ok(()) => true,
                    Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                };
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "fileinstall" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let ok = self.file_install(
                    &arg_str(&args, 0),
                    &arg_str(&args, 1),
                    arg_int(&args, 2) == 1,
                );
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }

            // ---------------- DllCallAddress / callbacks ----------------
            "dllcalladdress" => {
                // Without a PE loader there is no routine behind an address,
                // so the call fails rather than inventing a return value.
                ctx.set_error(1, 0);
                Value::Int(0)
            }
            "dllcallbackregister" => {
                let name = arg_str(&args, 0);
                if name.is_empty() {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                self.callbacks.push(Some(name));
                let ptr = CALLBACK_BASE + (self.callbacks.len() as i64 - 1) * 16;
                ctx.set_error(0, 0);
                Value::Int(ptr)
            }
            "dllcallbackgetptr" => {
                let handle = arg_int(&args, 0);
                let ok = callback_index(handle)
                    .is_some_and(|i| self.callbacks.get(i).is_some_and(|slot| slot.is_some()));
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(if ok { handle } else { 0 })
            }
            "dllcallbackfree" => {
                let handle = arg_int(&args, 0);
                let ok = callback_index(handle).is_some_and(|i| {
                    if i < self.callbacks.len() && self.callbacks[i].is_some() {
                        self.callbacks[i] = None;
                        true
                    } else {
                        false
                    }
                });
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }

            // ---------------- COM ----------------
            // There is no COM runtime off Windows, and handing back an object
            // would be an invented value, so these fail predictably and leave
            // the script's own error handling in charge.
            "objcreate" => {
                let progid = arg_str(&args, 0);
                match self.pseudo_com_create(&progid) {
                    Some(handle) => {
                        ctx.set_error(0, 0);
                        Value::obj(Rc::new(NativeObject {
                            name: progid,
                            handle,
                            release: None,
                        }))
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "objcreateinterface" | "objget" | "objevent" => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
            "objname" => {
                ctx.set_error(1, 0);
                Value::str("")
            }
            "isobj" => Value::Int(0),

            // ---------------- system information ----------------
            // A fixed machine profile, so a run is reproducible.
            "memgetstats" => {
                ctx.set_error(0, 0);
                Value::array(vec![
                    Value::Int(50),         // load, percent
                    Value::Int(8_388_608),  // total physical RAM, KB
                    Value::Int(4_194_304),  // available physical
                    Value::Int(16_777_216), // total pagefile
                    Value::Int(12_582_912), // available pagefile
                    Value::Int(2_097_152),  // total virtual
                    Value::Int(2_097_152),  // available virtual
                ])
            }
            "isadmin" => Value::Int(i64::from(self.is_admin)),

            // ---------------- drive mappings ----------------
            "drivemapadd" => {
                if !ctx.effect_allowed(EffectKind::NetAccess) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let requested = arg_str(&args, 0).trim().to_ascii_uppercase();
                let share = arg_str(&args, 1);
                if share.is_empty() {
                    ctx.set_error(5, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let device = if requested == "*" {
                    match self.free_drive_letter() {
                        Some(letter) => format!("{letter}:"),
                        None => {
                            ctx.set_error(4, 0);
                            return Ok(Some(Value::str("")));
                        }
                    }
                } else {
                    if !requested.is_empty() && !is_drive_device(&requested) {
                        ctx.set_error(4, 0);
                        return Ok(Some(Value::Int(0)));
                    }
                    requested.clone()
                };
                if !device.is_empty() && self.drive_maps.iter().any(|(d, _)| d == &device) {
                    ctx.set_error(3, 0);
                    return Ok(Some(Value::Int(0)));
                }
                self.drive_maps.push((device.clone(), share));
                ctx.set_error(0, 0);
                if requested == "*" {
                    Value::Str(device)
                } else {
                    Value::Int(1)
                }
            }
            "drivemapdel" => {
                if !ctx.effect_allowed(EffectKind::NetAccess) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let device = arg_str(&args, 0).trim().to_ascii_uppercase();
                let before = self.drive_maps.len();
                self.drive_maps.retain(|(d, _)| d != &device);
                let ok = self.drive_maps.len() != before;
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "drivemapget" => {
                let device = arg_str(&args, 0).trim().to_ascii_uppercase();
                match self.drive_maps.iter().find(|(d, _)| d == &device) {
                    Some((_, share)) => {
                        ctx.set_error(0, 0);
                        Value::Str(share.clone())
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::str("")
                    }
                }
            }
            "drivesetlabel" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let letter = arg_str(&args, 0)
                    .chars()
                    .next()
                    .map(|c| c.to_ascii_uppercase());
                match letter.and_then(|l| self.drives.iter_mut().find(|d| d.letter == l)) {
                    Some(drive) => {
                        drive.label = arg_str(&args, 1);
                        ctx.set_error(0, 0);
                        Value::Int(1)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }

            // ---------------- shell execution ----------------
            "shellexecute" => {
                if !ctx.effect_allowed(EffectKind::Spawn) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let ok = shell::spawn(
                    &arg_str(&args, 0),
                    &arg_str(&args, 1),
                    &arg_str(&args, 2),
                    0,
                )
                .is_ok();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "shellexecutewait" => {
                if !ctx.effect_allowed(EffectKind::Spawn) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let code = shell::spawn(
                    &arg_str(&args, 0),
                    &arg_str(&args, 1),
                    &arg_str(&args, 2),
                    0,
                )
                .and_then(|mut child| child.wait())
                .map(|status| i64::from(status.code().unwrap_or(0)));
                match code {
                    Ok(code) => {
                        ctx.set_error(0, 0);
                        Value::Int(code)
                    }
                    Err(_) => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "runas" => {
                if !ctx.effect_allowed(EffectKind::Spawn) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                // Credentials are accepted, not applied (see `shell`).
                let opt = arg_int(&args, 6);
                match shell::spawn(&arg_str(&args, 3), "", &arg_str(&args, 4), opt) {
                    Ok(child) => {
                        ctx.set_error(0, 0);
                        Value::Int(i64::from(child.id()))
                    }
                    Err(_) => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "runaswait" => {
                if !ctx.effect_allowed(EffectKind::Spawn) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let opt = arg_int(&args, 6);
                let code = shell::spawn(&arg_str(&args, 3), "", &arg_str(&args, 4), opt)
                    .and_then(|mut child| child.wait())
                    .map(|status| i64::from(status.code().unwrap_or(0)));
                match code {
                    Ok(code) => {
                        ctx.set_error(0, 0);
                        Value::Int(code)
                    }
                    Err(_) => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "shutdown" => {
                // Recorded, never acted on: a run must not power off the host.
                self.shutdowns += 1;
                ctx.set_error(0, 0);
                Value::Int(1)
            }
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
        "osarch" | "processorarch" | "cpuarch" => Value::Str(arch.as_str().to_string()),
        "oslang" | "oslocale" | "muilang" => Value::Str("0409".to_string()),
        "kblayout" => Value::Str("00000409".to_string()),
        "autoitx64" => Value::Int(i64::from(arch.pointer_size() == 8)),
        // ----- window show flags (`@SW_*`, as GUISetState/WinSetState take them) -----
        "sw_hide" => Value::Int(0),
        "sw_shownormal" => Value::Int(1),
        "sw_showminimized" => Value::Int(2),
        "sw_showmaximized" | "sw_maximize" => Value::Int(3),
        "sw_shownoactivate" => Value::Int(4),
        "sw_show" => Value::Int(5),
        "sw_minimize" => Value::Int(6),
        "sw_showminnoactive" => Value::Int(7),
        "sw_showna" => Value::Int(8),
        "sw_restore" => Value::Int(9),
        "sw_showdefault" => Value::Int(10),
        "sw_forceminimize" => Value::Int(11),
        // ----- display -----
        // The desktop is whatever the backend provides: a live window's
        // viewport, an offscreen renderer's canvas, or the emulated default.
        "desktopwidth" => Value::Int(i64::from(emu.gui.desktop_size().0)),
        "desktopheight" => Value::Int(i64::from(emu.gui.desktop_size().1)),
        // The assumed display mode of that desktop.
        "desktopdepth" => Value::Int(32),
        "desktoprefresh" => Value::Int(60),
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

/// Parse AutoIt's `^!+` hotkey notation into a Windows `HOTKEY` word.
fn parse_hotkey(s: &str) -> u16 {
    let mut modifiers: u16 = 0;
    let mut vk: u16 = 0;
    for c in s.chars() {
        match c {
            '^' => modifiers |= 2, // Ctrl
            '!' => modifiers |= 4, // Alt
            '+' => modifiers |= 1, // Shift
            c => {
                let up = c.to_ascii_uppercase();
                if up.is_ascii_alphanumeric() {
                    vk = up as u16;
                }
            }
        }
    }
    (modifiers << 8) | vk
}

/// The callback slot a `DllCallbackRegister` pointer refers to.
fn callback_index(handle: i64) -> Option<usize> {
    if handle < CALLBACK_BASE || (handle - CALLBACK_BASE) % 16 != 0 {
        return None;
    }
    Some(((handle - CALLBACK_BASE) / 16) as usize)
}

/// Whether `device` names a drive (`X:`) or a printer port (`LPT1:`).
fn is_drive_device(device: &str) -> bool {
    let bytes = device.as_bytes();
    if bytes.len() == 2 && bytes[1] == b':' {
        return bytes[0].is_ascii_alphabetic();
    }
    device.starts_with("LPT") || device.starts_with("COM")
}

/// A directory junction; a host symlink is the closest analogue.
#[cfg(unix)]
fn create_junction(target: &str, link: &str) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(unix))]
fn create_junction(_target: &str, _link: &str) -> bool {
    false
}

/// `C:` → `C:\` so the common-profile paths can be built from the drive.
fn drive_root(home_drive: &str) -> String {
    format!("{}\\", home_drive.trim_end_matches('\\'))
}

/// The length left after stripping valid PKCS#7 padding (`None` when the tail
/// is not padding, in which case the data is passed through untouched).
fn pkcs7_kept_len(data: &[u8]) -> Option<usize> {
    let pad = *data.last()? as usize;
    if pad == 0 || pad > 16 || pad > data.len() {
        return None;
    }
    if data[data.len() - pad..].iter().all(|b| *b as usize == pad) {
        Some(data.len() - pad)
    } else {
        None
    }
}

/// A `FindResourceW` selector from one argument: a string name, an integer id,
/// or `None` for a null argument.
fn resource_selector(v: Option<&Value>) -> Option<Selector> {
    match v {
        Some(Value::Str(s)) => Some(Selector::name(s.clone())),
        Some(other) if other.is_number() => {
            let id = other.to_int();
            (id != 0).then(|| Selector::id(id as u32))
        }
        _ => None,
    }
}

/// Find a struct handle among `DllCall`'s type/value argument pairs.
fn struct_handle_arg(extra: &[Value]) -> Option<i64> {
    extra.iter().find_map(|v| match v {
        Value::Int(i) if *i >= 1 => Some(*i),
        _ => None,
    })
}

/// Argument as a string, using AutoIt's coercion.
/// The `RegWrite` type argument: `"REG_SZ"`-style spellings first, then the
/// numeric code -- mirroring the native layer's `type_code_of`.
fn reg_type_code(v: &Value) -> Option<i64> {
    match v {
        Value::Str(text) => match text.trim().to_ascii_uppercase().as_str() {
            "REG_SZ" => Some(1),
            "REG_EXPAND_SZ" => Some(2),
            "REG_BINARY" => Some(3),
            "REG_DWORD" => Some(4),
            "REG_MULTI_SZ" => Some(7),
            "REG_QWORD" => Some(11),
            _ => None,
        },
        other => {
            let code = other.to_int();
            matches!(code, 1..=11).then_some(code)
        }
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

/// Argument as an integer.
fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
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

/// Resolve an environment variable the way a Windows-targeted script expects.
///
/// Windows spells its identity variables differently from a Unix host, so the
/// Windows-only names are mapped onto the same host values the emulated paths
/// already use (`@UserName` / `@ComputerName`, `WindowsPaths`); every other
/// name is read from the host environment unchanged. Names are matched
/// case-insensitively, as on Windows.
fn host_env(name: &str) -> Option<String> {
    match name.to_ascii_uppercase().as_str() {
        "USERNAME" => Some(host_user()),
        "COMPUTERNAME" => Some(host_computer()),
        _ => std::env::var(name).ok(),
    }
}
