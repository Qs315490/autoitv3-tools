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
//! | COM | no real runtime: `ObjCreate` answers `Scripting.Dictionary`, `WScript.Shell` and `Scripting.FileSystemObject` as plain objects over a member table ([`com`]); `IsObj` is `1` and `ObjName` echoes the ProgID for those; every other ProgID and `ObjGet`/`ObjCreateInterface`/`ObjEvent` fail with `@error = 1` rather than inventing objects |
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

mod bcrypt;
mod com;

use com::PseudoObject;
mod compress;
mod crypto;
pub mod dll;
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
    /// `bcrypt.dll` (CNG) objects: providers, hashes and keys.
    bcrypt: bcrypt::BcryptState,
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
            bcrypt: bcrypt::BcryptState::default(),
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










    // ----- pseudo COM -----



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




    // ----- DllCall -----



    // ----- CryptoAPI -----







    // ----- buffers -----






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
            // Off Windows there is no COM runtime; a handful of ProgIDs the
            // samples use are modelled as plain objects in `com`, and anything
            // else fails predictably so the script's own error handling stays
            // in charge.
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
            "objname" => match args.first() {
                Some(Value::Obj(o)) => Value::str(o.name.clone()),
                _ => {
                    ctx.set_error(1, 0);
                    Value::str("")
                }
            },
            "isobj" => Value::Int(i64::from(matches!(args.first(), Some(Value::Obj(_))))),

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
/// Whether a `DllCall` pointer argument was passed as null.
fn is_null_ptr(value: &Value) -> bool {
    matches!(value, Value::Null) || (matches!(value, Value::Int(_)) && value.to_int() == 0)
}

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
