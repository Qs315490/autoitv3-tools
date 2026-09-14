//! Windows platform implementation — the native Win32 backend.
//!
//! This module is compiled only on Windows targets (see
//! [`crate::host_platform`]). It answers the Windows-only library functions
//! that this toolset implements against the **real** operating system:
//!
//! * **native calls** — `DllCall`/`DllOpen`/`DllClose` through
//!   `LoadLibraryW`/`GetProcAddress`, and the `DllStruct*` family laid out by
//!   the same engine the emulation uses ([`autoitv3_platform::winemu`]'s
//!   `DllStruct`), but backed by *real* heap bytes so the pointer a
//!   `DllCall` receives is a pointer the callee can write through.
//! * **clipboard** — `ClipGet`/`ClipPut` via the real clipboard
//!   (`CF_UNICODETEXT`).
//! * **process** — `ProcessList`/`ProcessExists`/`ProcessClose` via the
//!   Toolhelp snapshot; `ProcessGetStats` in the common layer is also routed
//!   here for its memory figures.
//! * **drives** — the `DriveGet*` family via logical-drive and volume
//!   information APIs (`DriveMap*` is left to the emulation fallback).
//! * **system / shell** — `MemGetStats` (real `GlobalMemoryStatusEx`),
//!   `IsAdmin`, `ShellExecute*` (associations and verbs), `RunAs*` (real
//!   `CreateProcessWithLogonW` credentials), `DriveMap*` (WNet) and
//!   `Shutdown` (`ExitWindowsEx`), all profile-gated.
//! * **system macros** — the Windows-identity macros (`@WindowsDir`,
//!   `@OSVersion`, `@ComputerName`, …), answered before the common layer so
//!   its portable approximations do not win.
//!
//! * **registry** — the `Reg*` family against the real registry (64-bit view,
//!   AutoIt's type codes, profile-gated writes).
//! * **COM** — `ObjCreate`/`IsObj`/`ObjName` plus `.$member` / `.Method()`
//!   access through a hand-walked `IDispatch` vtable (`GetIDsOfNames` →
//!   `Invoke`). `ObjGet` (file monikers) and `ObjEvent` (event sinks) are
//!   not implemented and report `@error = 1`.
//!
//! * **system / shell** — `MemGetStats` (real `GlobalMemoryStatusEx`),
//!   `IsAdmin` (Administrators token membership), `ShellExecute*`
//!   (associations and verbs via `ShellExecuteExW`), `RunAs*` (real
//!   `CreateProcessWithLogonW` credentials), `DriveMap*` (WNet) and
//!   `Shutdown` (`ExitWindowsEx`), all profile-gated.
//!
//! Everything else Windows-specific that this layer does not implement — the
//! GUI family — falls through to the emulation layer when it is installed
//! (see [`crate::host_platform_with`]), and stays an undefined function when
//! it is not (`AU3_WIN_EMU=0` or `--no-win-emu`).
//!
//! # Deliberate approximations
//!
//! * Struct pointer layout follows the *host* pointer width: an `--win-arch
//!   x86` selection reshapes the emulation layer but not this one, which
//!   always matches the process that actually runs the script.
//! * `DllStructCreate($def, $ptr)` only maps over memory this layer already
//!   owns (an existing struct's bytes). A foreign raw pointer fails with
//!   `@error = 1` instead of aliasing unknown memory.
//! * Effect-gated calls (`ClipPut`, `ProcessClose`, `DriveSetLabel`) refuse
//!   under the deterministic analysis profile, like the common layer does.
//! * On x86 hosts only all-integer or all-float `DllCall` argument lists are
//!   supported (no mixed-class variadic path); x64 hosts support everything
//!   the type table knows.

pub(crate) mod clipboard;
pub(crate) mod com;
pub(crate) mod dll;
mod drive;
mod files;
pub(crate) mod misc;
pub(crate) mod process;
pub(crate) mod registry;

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::EffectKind;
use autoitv3_runtime::value::Value;

use crate::winfmt::DllStruct;

use crate::winfmt::WindowsArch;

/// The platform used when the target OS is Windows.
#[derive(Debug, Default)]
pub struct WindowsPlatform {
    /// Whether a COM apartment was initialized (`ObjCreate`).
    #[allow(unused)]
    com_initialized: bool,
    /// Every live `DllStruct`, 1-based handle = index + 1. Freed slots are
    /// reused so handles stay small.
    structs: Vec<Option<DllStruct>>,
    /// `DllOpen` results (real module handles as raw addresses), 1-based.
    modules: Vec<Option<usize>>,
    /// `DllCall` targets seen but not resolvable, for `AU3_WINEMU_TRACE`.
    unimplemented_dll_calls: Vec<String>,
    /// PE image `GetModuleHandleW(NULL)` should report while analysing a
    /// compiled script — the image under analysis, not the `au3` host.
    resource_module: Option<std::path::PathBuf>,
    /// Handle of `resource_module` once mapped as an image resource; 0 means
    /// "not loaded yet".
    resource_base: usize,
}

impl WindowsPlatform {
    /// Create the Windows platform.
    pub fn new() -> Self {
        Self::default()
    }

    /// Point `GetModuleHandleW(NULL)` at the PE image under analysis.
    ///
    /// A compiled AutoIt script reads its payload out of *its own* image
    /// (`GetModuleHandleW(NULL)` → `FindResourceW` → `SizeofResource` →
    /// `LoadResource` → `LockResource`). When the tool analyses the extracted
    /// source, the real host process is `au3`, whose image carries none of
    /// those resources, so the lookup fails and the script's decoder returns
    /// an error value. Naming the image the script was compiled into restores
    /// the script's own view of "the current module".
    pub fn with_resource_module(mut self, path: impl AsRef<std::path::Path>) -> Self {
        self.resource_module = Some(path.as_ref().to_path_buf());
        self
    }

    /// The handle a `GetModuleHandleW/A(NULL)` call must report, or `None`
    /// when this is not such a call or no resource image is configured.
    ///
    /// The image is mapped lazily on first use with
    /// `LOAD_LIBRARY_AS_IMAGE_RESOURCE`, which exposes its resources without
    /// running any of its code.
    pub(crate) fn resource_base_for_null_lookup(
        &mut self,
        function: &str,
        args: &[(String, Value)],
    ) -> Option<usize> {
        let function = function.to_ascii_lowercase();
        if !matches!(
            function.as_str(),
            "getmodulehandlew" | "getmodulehandlea" | "getmodulehandle"
        ) {
            return None;
        }
        match args.first() {
            Some((_, value)) if !is_null_module_name(value) => return None,
            _ => {}
        }
        let path = self.resource_module.clone()?;
        if self.resource_base != 0 {
            return Some(self.resource_base);
        }
        let base = dll::load_library_as_image_resource(&path);
        if base == 0 {
            return None;
        }
        self.resource_base = base;
        Some(base)
    }

    /// The architecture this process runs as — what `DllStruct` layouts and
    /// pointer arguments must match.
    pub(crate) fn arch(&self) -> WindowsArch {
        if cfg!(target_pointer_width = "64") {
            WindowsArch::X64
        } else {
            WindowsArch::X86
        }
    }

    /// Store a freshly created struct and return its 1-based handle.
    pub(crate) fn push_struct(&mut self, s: DllStruct) -> i64 {
        if let Some(i) = self.structs.iter().position(|slot| slot.is_none()) {
            self.structs[i] = Some(s);
            return i as i64 + 1;
        }
        self.structs.push(Some(s));
        self.structs.len() as i64
    }

    pub(crate) fn struct_ref(&self, handle: i64) -> Option<&DllStruct> {
        if handle < 1 {
            return None;
        }
        self.structs.get(handle as usize - 1)?.as_ref()
    }

    /// Resolve a struct by its handle *or* by an address inside it — the two
    /// forms a script hands to `DllCall` and friends.
    pub(crate) fn struct_any_mut(&mut self, value: i64) -> Option<&mut DllStruct> {
        if value >= 1 {
            let index = value as usize - 1;
            if self.structs.get_mut(index).is_some_and(|s| s.is_some()) {
                return self.structs[index].as_mut();
            }
        }
        let addr = value as u64;
        let found = self.structs.iter().position(|slot| {
            slot.as_ref().is_some_and(|s| {
                s.address() != 0 && addr >= s.address() && addr < s.address() + s.size() as u64
            })
        })?;
        self.structs[found].as_mut()
    }

    /// Remember a module handle handed out by `DllOpen`. `DllOpen` returns the
    /// **real** module address — scripts pass it to `DllCall` pointer
    /// arguments — so the table only tracks liveness for `DllClose`.
    pub(crate) fn track_module(&mut self, module: usize) {
        self.modules.push(Some(module));
    }

    pub(crate) fn close_module(&mut self, handle: i64) -> bool {
        if handle < 1 {
            return false;
        }
        if let Some(i) = self
            .modules
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|m| *m == handle as usize))
        {
            let m = self.modules[i].take().unwrap_or(0);
            if m != 0 {
                dll::free_library(m);
                return true;
            }
        }
        false
    }

    /// Record a `DllCall` target this layer could not resolve, once each, and
    /// report it on stderr when `AU3_WINEMU_TRACE` is set — the same channel
    /// the emulation layer uses.
    pub(crate) fn note_unimplemented_call(&mut self, dll: &str, function: &str) {
        let target = format!("{dll}!{function}");
        if self.unimplemented_dll_calls.iter().any(|t| *t == target) {
            return;
        }
        self.unimplemented_dll_calls.push(target.clone());
        if std::env::var_os("AU3_WINEMU_TRACE").is_some_and(|v| v != "0") {
            eprintln!("[win32] DllCall not resolved: {target}");
        }
    }
}

/// Whether a `GetModuleHandleW`/`GetModuleHandleA` argument names the current
/// module (`NULL`, `0` or the empty string) as opposed to a module name.
fn is_null_module_name(value: &Value) -> bool {
    match value {
        Value::Null | Value::Int(0) | Value::Bool(false) => true,
        Value::Str(s) => s.is_empty(),
        _ => false,
    }
}

/// Whether the current profile allows side effects.
impl Platform for WindowsPlatform {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn provides(&self, name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            // native calls
            "dllcall" | "dllcalladdress" | "dllopen" | "dllclose"
                | "dllstructcreate" | "dllstructgetdata" | "dllstructsetdata"
                | "dllstructgetsize" | "dllstructgetptr" | "isdllstruct"
                // clipboard
                | "clipget" | "clipput"
                // registry
                | "regread" | "regwrite" | "regdelete"
                | "regenumkey" | "regenumval"
                // COM
                | "objcreate" | "objget" | "objevent" | "isobj" | "objname"
                // process
                | "processlist" | "processexists" | "processclose"
                // drives
                | "drivegetdrive" | "drivegettype" | "drivegetfilesystem"
                | "drivegetlabel" | "drivegetserial" | "drivegetspacetotal"
                | "drivegetspacefree" | "drivegetstatus" | "drivesetlabel"
                // system / shell
                | "memgetstats" | "isadmin" | "shellexecute" | "shellexecutewait"
                | "runas" | "runaswait" | "drivemapadd" | "drivemapdel"
                | "drivemapget" | "shutdown"
                // real Windows filesystem semantics the common layer only
                // approximates
                | "filegetattrib" | "filesetattrib" | "filegetshortname"
                | "envupdate"
        )
    }

    fn macro_value(&self, name: &str) -> Option<Value> {
        self::system_macro(name)
    }

    fn obj_create(
        &mut self,
        name: &str,
        args: &[Value],
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        self.com_initialized = true;
        com::obj_create(name, args).map(Some).map_err(|_| RuntimeError::Unsupported {
            what: format!("ObjCreate({name:?})"),
            span: None,
        })
    }

    fn obj_get(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        com::obj_get(obj, member).map(Some).map_err(|e| RuntimeError::Unsupported {
            what: e,
            span: None,
        })
    }

    fn obj_set(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        value: &Value,
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        com::obj_set(obj, member, value).map(Some).map_err(|e| RuntimeError::Unsupported {
            what: e,
            span: None,
        })
    }

    fn obj_call(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        args: &[Value],
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        com::obj_call(obj, member, args).map(Some).map_err(|e| RuntimeError::Unsupported {
            what: e,
            span: None,
        })
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let value = match name.to_ascii_lowercase().as_str() {
            // ---------------- DllStruct ----------------
            "dllstructcreate" => self.struct_create(&args, ctx),
            "dllstructgetsize" => self.struct_size(&args, ctx),
            "dllstructgetptr" => self.struct_ptr(&args, ctx),
            "isdllstruct" => {
                let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
                Value::Int(i64::from(self.struct_ref(handle).is_some()))
            }
            "dllstructgetdata" | "dllstructsetdata" => {
                self.struct_data(&args, ctx, name.eq_ignore_ascii_case("DllStructGetData"))
            }
            // ---------------- DllCall ----------------
            "dllcall" => self.dll_call(&args, ctx),
            "dllcalladdress" => self.dll_call_address(&args, ctx),
            "dllopen" => self.dll_open(&args, ctx),
            "dllclose" => self.dll_close(&args, ctx),
            // ---------------- registry ----------------
            "regread" => {
                let key = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
                let value = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
                match registry::reg_read(&key, &value) {
                    Ok((v, kind)) => {
                        ctx.set_error(0, kind);
                        v
                    }
                    Err(code) => {
                        ctx.set_error(code, 0);
                        Value::str("")
                    }
                }
            }
            "regwrite" => registry::reg_write(&args, ctx),
            // ---------------- real Windows filesystem semantics ----------------
            "filegetattrib" => {
                let path = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
                match files::file_get_attrib(&path) {
                    Some(a) => {
                        ctx.set_error(0, 0);
                        Value::Str(a)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::str("")
                    }
                }
            }
            "filesetattrib" => {
                if !ctx.effect_allowed(EffectKind::FileWrite) {
                    ctx.set_error(1, 0);
                    return Ok(Some(Value::Int(0)));
                }
                let path = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
                let changes = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
                let ok = !changes.is_empty() && files::file_set_attrib(&path, &changes);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "filegetshortname" => {
                let path = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
                ctx.set_error(0, 0);
                Value::Str(files::file_get_short_name(&path))
            }
            "envupdate" => files::env_update(),
            // ---------------- COM ----------------
            "objcreate" => {
                let name = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
                match com::obj_create(&name, args.get(1..).unwrap_or(&[])) {
                    Ok(v) => {
                        ctx.set_error(0, 0);
                        v
                    }
                    Err(_) => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "objget" => {
                // `ObjGet` (object from file / moniker) is not implemented.
                ctx.set_error(1, 0);
                Value::Int(0)
            }
            "objevent" => {
                // Event sinks need a message-pumping connection point.
                ctx.set_error(1, 0);
                Value::Null
            }
            "isobj" => {
                Value::Int(i64::from(
                    args.first().is_some_and(|v| com::is_obj(v)),
                ))
            }
            "objname" => match args.first() {
                Some(Value::Obj(o)) => Value::Str(com::obj_name(o)),
                _ => {
                    ctx.set_error(1, 0);
                    Value::str("")
                }
            },
            "regdelete" => registry::reg_delete(&args, ctx),
            "regenumkey" => registry::reg_enum_key(&args, ctx),
            "regenumval" => registry::reg_enum_val(&args, ctx),
            // ---------------- clipboard ----------------
            "clipget" => clipboard::clip_get(ctx),
            "clipput" => clipboard::clip_put(&args, ctx),
            // ---------------- process ----------------
            "processlist" => process::process_list(),
            "processexists" => process::process_exists(&args),
            "processclose" => process::process_close(&args, ctx),
            // ---------------- drives ----------------
            "drivegetdrive" => drive::drive_get_drive(&args, ctx),
            "drivegettype" => drive::drive_get_type(&args, ctx),
            "drivegetfilesystem" => drive::drive_get_field(&args, ctx, drive::DriveField::FileSystem),
            "drivegetlabel" => drive::drive_get_field(&args, ctx, drive::DriveField::Label),
            "drivegetserial" => drive::drive_get_field(&args, ctx, drive::DriveField::Serial),
            "drivegetspacetotal" => drive::drive_get_field(&args, ctx, drive::DriveField::SpaceTotal),
            "drivegetspacefree" => drive::drive_get_field(&args, ctx, drive::DriveField::SpaceFree),
            "drivegetstatus" => drive::drive_get_status(&args, ctx),
            "drivesetlabel" => drive::drive_set_label(&args, ctx),
            // ---------------- system / shell ----------------
            "memgetstats" => misc::mem_get_stats(ctx),
            "isadmin" => misc::is_admin(),
            "shellexecute" => misc::shell_execute(&args, ctx),
            "shellexecutewait" => misc::shell_execute_wait(&args, ctx),
            "runas" => misc::run_as(&args, ctx, false),
            "runaswait" => misc::run_as(&args, ctx, true),
            "drivemapadd" => misc::drive_map_add(&args, ctx),
            "drivemapdel" => misc::drive_map_del(&args, ctx),
            "drivemapget" => misc::drive_map_get(&args, ctx),
            "shutdown" => misc::shutdown(&args, ctx),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}

// ---------------------------------------------------------------------------
// Windows-identity macros
// ---------------------------------------------------------------------------

/// The macros whose honest answer needs the real OS: directory layout,
/// machine identity, and OS version. Answered before the common layer so its
/// portable approximations (`%HOME`, XDG paths) do not win on Windows.
fn system_macro(name: &str) -> Option<Value> {
    let dir = |p: String| {
        let mut s = p;
        if !s.ends_with('\\') {
            s.push('\\');
        }
        Value::Str(s)
    };
    let env = |key: &str| std::env::var(key).unwrap_or_default();
    let value = match name.to_ascii_lowercase().as_str() {
        "windowsdir" => {
            let mut buf = [0u16; 261];
            let n = unsafe {
                windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
                    buf.as_mut_ptr(),
                    261,
                )
            };
            dir(String::from_utf16_lossy(&buf[..n as usize]))
        }
        "systemdir" => {
            let mut buf = [0u16; 261];
            let n = unsafe {
                windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                    buf.as_mut_ptr(),
                    261,
                )
            };
            dir(String::from_utf16_lossy(&buf[..n as usize]))
        }
        "tempdir" => {
            let mut buf = [0u16; 261];
            let n = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetTempPathW(261, buf.as_mut_ptr())
            };
            dir(String::from_utf16_lossy(&buf[..n as usize]))
        }
        "homedrive" => Value::Str(env("SystemDrive")),
        "homepath" => Value::Str(env("HomePath")),
        "homeshare" => Value::Str(env("HomeShare")),
        "userprofiledir" => dir(env("USERPROFILE")),
        "appdatadir" => dir(env("APPDATA")),
        "localappdatadir" => dir(env("LOCALAPPDATA")),
        "programfilesdir" => dir(env("ProgramFiles")),
        "commonfilesdir" => dir(format!("{}\\Common Files", env("ProgramFiles"))),
        "desktopdir" => dir(format!("{}\\Desktop", env("USERPROFILE"))),
        "mydocumentsdir" => dir(format!("{}\\Documents", env("USERPROFILE"))),
        "startmenudir" => dir(format!("{}\\Microsoft\\Windows\\Start Menu", env("APPDATA"))),
        "startupdir" => dir(format!(
            "{}\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
            env("APPDATA")
        )),
        "programsdir" => dir(format!(
            "{}\\Microsoft\\Windows\\Start Menu\\Programs",
            env("APPDATA")
        )),
        "username" => Value::Str(env("USERNAME")),
        "computername" => {
            let mut buf = [0u16; 64];
            let mut len = buf.len() as u32;
            let ok = unsafe {
                windows_sys::Win32::System::SystemInformation::GetComputerNameExW(
                    windows_sys::Win32::System::SystemInformation::ComputerNameNetBIOS,
                    buf.as_mut_ptr(),
                    &mut len,
                )
            } != 0;
            Value::Str(if ok {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            })
        }
        "osversion" | "osbuild" | "osarch" | "cpuarch" | "osservicepack" => {
            let Some(version) = os_version() else {
                return None;
            };
            match name.to_ascii_lowercase().as_str() {
                "osversion" => Value::Str(version.name().to_string()),
                "osbuild" => Value::Int(version.build as i64),
                "osservicepack" => Value::Str(String::from_utf16_lossy(
                    &version.service_pack[..version
                        .service_pack
                        .iter()
                        .position(|u| *u == 0)
                        .unwrap_or(0)],
                )),
                // The OS arch as this process sees it: this toolset's build
                // width is the width the script's DllStruct layout uses too.
                "osarch" | "cpuarch" => Value::str(if cfg!(target_pointer_width = "64") {
                    "X64"
                } else {
                    "X86"
                }),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(value)
}

/// The real OS version via `RtlGetVersion` — the only API that reports
/// Windows 10/11 truthfully without a manifest.
fn os_version() -> Option<OsVersion> {
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    let ntdll = dll::load_library("ntdll.dll");
    let address = dll::load_function(ntdll, "RtlGetVersion")?;
    let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    let f: unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32 =
        unsafe { std::mem::transmute(address) };
    if unsafe { f(&mut info) } != 0 {
        return None;
    }
    Some(OsVersion {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
        service_pack: info.szCSDVersion,
    })
}

struct OsVersion {
    major: u32,
    minor: u32,
    build: u32,
    service_pack: [u16; 128],
}

impl OsVersion {
    /// AutoIt's `@OSVersion` spelling.
    fn name(&self) -> &'static str {
        match (self.major, self.minor, self.build) {
            (5, 0, _) | (5, 1, _) | (5, 2, _) => "WIN_XP",
            (6, 0, _) => "WIN_VISTA",
            (6, 1, _) => "WIN_7",
            (6, 2, _) => "WIN_8",
            (6, 3, _) => "WIN_81",
            (10, 0, build) if build >= 22000 => "WIN_11",
            (10, 0, _) => "WIN_10",
            _ => "WIN_10",
        }
    }
}

// ---------------------------------------------------------------------------
// DllStruct handle plumbing (the layout engine is the shared winemu one)
// ---------------------------------------------------------------------------

impl WindowsPlatform {
    fn struct_create(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let definition = args
            .first()
            .map(|v| v.to_autoit_string())
            .unwrap_or_default();
        // `DllStructCreate($def, $ptr)` maps over memory this layer owns —
        // a pointer handed out by `DllStructGetPtr`, so both views alias.
        let pointer = args.get(1).map(|v| v.to_int()).unwrap_or(0);
        let created = match self.struct_any_mut(pointer) {
            Some(existing) => {
                let base = existing.address();
                let (storage, base_offset) = existing.storage();
                let rel = (pointer as u64).saturating_sub(base) as usize;
                DllStruct::create_over(
                    &definition,
                    self.arch(),
                    storage,
                    base_offset + rel,
                    pointer as u64,
                )
            }
            None if pointer == 0 => DllStruct::create(&definition, self.arch()),
            // A raw pointer we do not own cannot be aliased safely.
            None => {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
        };
        match created {
            Ok(mut s) => {
                if s.address() == 0 {
                    // The backing buffer is allocated at exactly `size` bytes
                    // and never reallocated, so its heap pointer is stable and
                    // real — native callees write through it directly.
                    s.set_address(s.real_address());
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

    fn struct_size(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
        match self.struct_any_mut(handle) {
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

    fn struct_ptr(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
        match self.struct_any_mut(handle) {
            Some(s) => {
                let address = s.address() as i64;
                ctx.set_error(0, 0);
                Value::Int(address)
            }
            None => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    fn struct_data(&mut self, args: &[Value], ctx: &mut dyn HostContext, is_get: bool) -> Value {
        use crate::winfmt::FieldSelector;
        let handle = args.first().map(|v| v.to_int()).unwrap_or(0);
        let selector = args
            .get(1)
            .map(FieldSelector::from_value)
            .unwrap_or(FieldSelector::Index(0));
        // `DllStructGetData(Struct, Element [, Index])` and
        // `DllStructSetData(Struct, Element, Value [, Index])` put the array
        // index in different positions.
        let element = args
            .get(if is_get { 2 } else { 3 })
            .map(|v| v.to_int())
            .filter(|n| *n >= 1);
        let Some(s) = self.struct_any_mut(handle) else {
            ctx.set_error(1, 0);
            return if is_get { Value::str("") } else { Value::Int(0) };
        };
        let Some(field) = s.field_index(&selector) else {
            ctx.set_error(1, 0);
            return if is_get { Value::str("") } else { Value::Int(0) };
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
}
