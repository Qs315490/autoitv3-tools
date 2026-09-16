//! `autoitv3-platform` — operating-system integrations for `autoitv3-runtime`.
//!
//! AutoIt v3 is a Windows automation language: a good part of its function
//! library is a thin wrapper over the Win32 API. The interpreter core, the
//! value model and the language-level builtins therefore stay platform-neutral,
//! and the library functions are layered here:
//!
//! | layer | module | installed on | contents |
//! |---|---|---|---|
//! | common | [`common`] | **every** platform | file/directory I/O, INI, environment, math, timers, console, **and** process execution (`Run`/`StdoutRead`/…) plus networking (`TCP*`/`UDP*`/`Inet*`) — the parts AutoIt does the same way everywhere |
//! | emulation | [`winemu`] | non-Windows | Windows-flavoured identity, paths, `DllStruct*`/`DllCall`, registry, clipboard and drives, so a Windows-targeted script keeps running off Windows |
//! | system | [`linux`] / [`windows`] | one OS each | what genuinely differs: process queries on Linux; native `DllCall`/`DllStruct`, clipboard, process and drive queries plus OS-identity macros on Windows |
//!
//! [`host_platform`] builds the right stack for the target at compile time; a
//! build only ever compiles its own system layer. The layers are tried in order
//! by [`CompositePlatform`], so the common functions are available on Windows
//! too and a system layer only has to add what is actually system-specific.
//!
//! [`elevate`] is outside those layers: it is about *this process*, not about
//! the script's library functions — the interpreter uses it to honour
//! `#RequireAdmin` by starting an elevated copy of itself.
//!
//! On non-Windows targets the stack is **emulation → common → linux**: the
//! [`WindowsEmulation`] layer answers first (it deliberately shadows the
//! common directory macros), and anything it does not know falls through to
//! the host. On Windows targets it is **windows → common → emulation**: the
//! native layer answers with real Win32 semantics, and the emulation sits at
//! the end as a fallback for the Windows-only names it does not implement.
//! Set `AU3_WIN_EMU=0` — or install a disabled [`WindowsEmulation`] with
//! [`host_platform_with`] — for the plain native stacks, where the emulated
//! semantics are skipped.
//!
//! # Layering with the runtime
//!
//! The [`Platform`] *trait* lives in `autoitv3-runtime` (it is the seam the
//! interpreter calls into) while the *implementations* live here. The
//! dependency direction is one-way — this crate depends on the runtime, never
//! the reverse — so the core names no concrete operating system.
//!
//! # Using it
//!
//! ```
//! let prog = autoitv3_ast::parse("Func F()\n    Return 1\nEndFunc\n").unwrap();
//! let mut rt = autoitv3_platform::runtime_with_platform(&prog);
//! assert!(rt.platform_name().contains("common"));
//! ```
//!
//! Lookup order for a call the interpreter cannot resolve itself is
//! **builtins → host → platform**, so an embedding application's
//! [`Host`](autoitv3_runtime::host::Host) can always override a
//! platform-provided function.

pub mod dialog_notice;
pub mod elevate;
pub mod linux;
pub mod common;
pub mod pathmap;
pub mod winemu;

/// `DllCall` type-token parsing (`"INT:cdecl"`). Only the native Windows
/// backend consults it, but the spelling rules are worth testing on every
/// host, so the module is built for the crate's tests everywhere.
#[cfg(any(windows, test))]
pub(crate) mod abi;

/// Pure Windows file-format / binary-layout machinery (DllStruct layouts,
/// PE resources, RT_VERSION, Shell Links) shared by the answering layers.
pub mod winfmt;

#[cfg(windows)]
pub mod windows;

pub use linux::LinuxPlatform;
pub use pathmap::PathMap;
pub use common::CommonPlatform;
pub use winemu::{
    find_resource_module, has_staged_resources, resource_search_dirs, CipherAlg, FileRegistry,
    HashAlg, MemoryRegistry, RegistryData, RegistryStore, WindowsEmulation, WindowsVersion,
};
pub use winfmt::{DllStruct, FieldSelector, PeImage, Resource, Selector, Shortcut, WindowsArch};

#[cfg(windows)]
pub use windows::WindowsPlatform;

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::value::Value;
use autoitv3_runtime::Runtime;

/// The Windows-native GUI backend, when this host has one.
///
/// That is the real Win32 one: a script's window is an ordinary native window
/// and its controls are real Win32 children, drawn and hit-tested by the OS.
/// The GUI *semantics* still come from the emulation layer — every host runs
/// the same 165 functions — but there is no toolkit to pull in and nothing to
/// opt into: a script's GUI looks native because it *is* native.
///
/// `Some` on Windows, where `auto` already installs it and a caller can ask for
/// it by name (`au3 run --gui native`); `None` elsewhere, where there is no
/// native Win32 to draw on. The alternatives off Windows are
/// `autoitv3-gui-egui`'s offscreen `EguiBackend` (`gui-egui`), its live window
/// (`gui-window`), or an embedder's own backend through
/// [`WindowsEmulation::with_gui_backend`].
pub fn native_gui_backend() -> Option<Box<dyn winemu::GuiBackend>> {
    #[cfg(windows)]
    {
        Some(Box::new(windows::gui::Win32Backend::new()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The GUI backend a fresh [`WindowsEmulation`] starts with.
///
/// [`native_gui_backend`] where there is one, and
/// [`HeadlessBackend`](winemu::HeadlessBackend) where there is not: off Windows
/// no window system is assumed, so nothing is drawn until a renderer is
/// installed with [`WindowsEmulation::with_gui_backend`].
pub(crate) fn default_gui_backend() -> Box<dyn winemu::GuiBackend> {
    match native_gui_backend() {
        Some(backend) => backend,
        None => Box::new(winemu::HeadlessBackend::new()),
    }
}

/// AutoIt's shape for a directory macro (`@WindowsDir`, `@ScriptDir`, ...).
///
/// No trailing separator — `@WindowsDir` is `C:\Windows`, `@TempDir` is
/// `...\Temp`, and a script in `C:\tmp` has `@ScriptDir` = `C:\tmp`. The one
/// exception is the root of a drive, which *keeps* it: the help page says
/// `@ScriptDir`/`@WorkingDir` "only include a trailing backslash when the
/// script is located in the root of a drive", and `C:\` would otherwise lose
/// the separator that makes it a directory.
///
/// Getting this wrong is not cosmetic: a script that compares a path it built
/// (`$drive & "\Windows"`) with `@WindowsDir` — which is how a driver installer
/// looks for a Windows installation — fails when the macro carries a separator
/// the script did not write.
pub(crate) fn directory_macro(path: &str, separator: char) -> String {
    let trimmed = path.trim_end_matches(separator);
    if trimmed.is_empty() {
        // A POSIX root stays a root.
        return path.to_string();
    }
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        // A bare drive is a root too: `C:` is spelled `C:\`.
        return format!("{trimmed}{separator}");
    }
    trimmed.to_string()
}

/// The common layer, wired to the emulation's drive map and script path.
///
/// The two travel together: the emulation answers `C:\...` paths and reports
/// script macros in the same spelling, and this is the layer that actually
/// touches the host filesystem, so it needs the map to translate them back. On
/// Windows the common layer sits *above* the emulation, so it is also the one
/// that answers `@ScriptDir` there.
fn common_layer(emulation: &WindowsEmulation) -> CommonPlatform {
    let mut common = CommonPlatform::new();
    // A disabled emulation means "plain host semantics", which includes plain
    // host paths: `AU3_WIN_EMU=0` must not leave a `C:\` translation behind.
    if !emulation.is_enabled() {
        return common;
    }
    if let Some(map) = emulation.path_map() {
        common = common.with_path_map(map.clone());
    }
    if let Some(script) = emulation.script_path() {
        common = common.with_script_path(script);
    }
    common
}

/// Several platforms tried in order.
///
/// This is how the common layer and the system layer compose: the first layer
/// that provides a name answers it, and a layer that does not know the name
/// says so with `Ok(None)`.
pub struct CompositePlatform {
    name: &'static str,
    layers: Vec<Box<dyn Platform>>,
}

impl CompositePlatform {
    /// Build a composite with an explicit name and layer order.
    pub fn new(name: &'static str, layers: Vec<Box<dyn Platform>>) -> Self {
        Self { name, layers }
    }

    /// The layers, in lookup order.
    pub fn layers(&self) -> &[Box<dyn Platform>] {
        &self.layers
    }
}

impl Platform for CompositePlatform {
    fn name(&self) -> &'static str {
        self.name
    }

    fn provides(&self, name: &str) -> bool {
        self.layers.iter().any(|p| p.provides(name))
    }

    /// Ask each layer in order, so the common layer can answer the
    /// environment macros and the system layer the OS-identity ones.
    fn macro_value(&self, name: &str) -> Option<Value> {
        self.layers.iter().find_map(|p| p.macro_value(name))
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        for layer in &mut self.layers {
            // `provides` is a cheap, clone-free pre-filter: only the layer
            // that will actually answer pays for the argument clone.
            if !layer.provides(name) {
                continue;
            }
            if let Some(v) = layer.call(name, args.clone(), ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Merge the callback invocations every layer scheduled; the runtime
    /// runs them after this call returns.
    fn take_pending_callbacks(&mut self) -> Vec<(String, Vec<Value>)> {
        let mut all = Vec::new();
        for layer in &mut self.layers {
            all.extend(layer.take_pending_callbacks());
        }
        all
    }

    /// Forward object creation to the first layer that answers.
    fn obj_create(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        for layer in &mut self.layers {
            if let Some(v) = layer.obj_create(name, args, ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn obj_get(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        for layer in &mut self.layers {
            if let Some(v) = layer.obj_get(obj, member, ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn obj_set(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        value: &Value,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        for layer in &mut self.layers {
            if let Some(v) = layer.obj_set(obj, member, value, ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn obj_call(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        for layer in &mut self.layers {
            if let Some(v) = layer.obj_call(obj, member, args, ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }
}

/// The platform stack for the operating system this build targets.
///
/// On a Windows build this is `windows+common+winemu`: the native
/// [`WindowsPlatform`] answers first, the common layer carries the portable
/// functions, and the [`WindowsEmulation`] fallback covers the Windows-only
/// names the native layer does not implement. On every other target it is
/// `winemu+common+linux`: the [`WindowsEmulation`] layer is configured from
/// the environment (`AU3_WIN_VERSION`, `AU3_WIN_ARCH`, `AU3_WIN_EMU`) and
/// defaults to emulating **Windows 10 x64**.
///
/// Use [`host_platform_with`] when the emulation has to be configured in code
/// (the CLI's `--win-version`, a test, an embedding application).
pub fn host_platform() -> Box<dyn Platform> {
    host_platform_with(WindowsEmulation::from_env())
}

/// Like [`host_platform`], but with an explicit [`WindowsEmulation`] layer.
///
/// On every target the layer is configured by the caller (the CLI's
/// `--win-version`, a test, an embedding application) and a disabled
/// emulation is left out entirely.
///
/// On non-Windows targets the emulation is installed **first** so its macros
/// win over the common ones. On Windows targets it is installed **last** as a
/// fallback: the native [`WindowsPlatform`] and the common layer answer with
/// real semantics first, and the emulation only catches the Windows-only
/// names neither implements (registry, GUI, `DriveMap*`, …) — the same names
/// that stay undefined functions with `AU3_WIN_EMU=0` / `--no-win-emu`. The
/// emulation's directory macros therefore stay dormant on Windows: the common
/// layer's real paths win.
pub fn host_platform_with(emulation: WindowsEmulation) -> Box<dyn Platform> {
    let emulated = emulation.is_enabled();
    // The drive map is shared with the common layer: the emulation hands the
    // script `C:\` paths and the layer that touches the filesystem has to turn
    // them back into host paths (see [`pathmap`]).
    let common = common_layer(&emulation);
    #[cfg(windows)]
    {
        let mut native = windows::WindowsPlatform::new();
        if let Some(path) = emulation.module_path() {
            // `GetModuleHandleW(NULL)` must name the image under analysis, not
            // the host `au3` process, or the script's own resources vanish.
            native = native.with_resource_module(path);
        }
        let mut layers: Vec<Box<dyn Platform>> = vec![Box::new(native), Box::new(common)];
        if emulated {
            layers.push(Box::new(emulation));
        }
        let name = if emulated {
            "windows+common+winemu"
        } else {
            "windows+common"
        };
        Box::new(CompositePlatform::new(name, layers))
    }
    #[cfg(not(windows))]
    {
        let mut layers: Vec<Box<dyn Platform>> = Vec::new();
        if emulated {
            layers.push(Box::new(emulation));
        }
        layers.push(Box::new(common));
        layers.push(Box::new(LinuxPlatform::new()));
        let name = if emulated {
            "winemu+common+linux"
        } else {
            "common+linux"
        };
        Box::new(CompositePlatform::new(name, layers))
    }
}

/// Fine-grained platform-stack options.
///
/// The presets stay what they are ([`host_platform`] / [`host_platform_with`]);
/// this adds two dials on top:
///
/// * `emulation` — the same [`WindowsEmulation`] configuration
///   `host_platform_with` takes;
/// * `force_emulated` — function names the **native** Windows layer must
///   decline, so the emulation layer answers them instead. This is how a run
///   opts into per-area emulation (the CLI's `--emulate registry` spells
///   `Reg*` names) while everything else keeps native semantics.
pub struct PlatformOptions {
    /// Emulation-layer configuration (`WindowsEmulation::from_env()` by
    /// convention).
    pub emulation: WindowsEmulation,
    /// Lower-case function names routed to the emulation layer even when the
    /// native layer could answer. Empty by default.
    pub force_emulated: Vec<String>,
    /// Answer `IsAdmin()` as an elevated user even though this process is not
    /// one.
    ///
    /// A simulation knob: the CLI sets it when a script declares
    /// `#RequireAdmin` and the run is a deterministic analysis, where really
    /// elevating (a consent prompt and a second process) is a side effect the
    /// profile exists to refuse. The script then takes its "already an
    /// administrator" path without anything being raised.
    pub assume_admin: bool,
}

/// A [`WindowsPlatform`] that declines the names in `declined`, letting the
/// layers behind it answer instead.
#[cfg(windows)]
struct FilteredPlatform {
    inner: windows::WindowsPlatform,
    declined: std::rc::Rc<[String]>,
}

#[cfg(windows)]
impl Platform for FilteredPlatform {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn provides(&self, name: &str) -> bool {
        !self
            .declined
            .iter()
            .any(|d| d.eq_ignore_ascii_case(name)) && self.inner.provides(name)
    }

    fn macro_value(&self, name: &str) -> Option<Value> {
        self.inner.macro_value(name)
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        if self
            .declined
            .iter()
            .any(|d| d.eq_ignore_ascii_case(name))
        {
            return Ok(None);
        }
        self.inner.call(name, args, ctx)
    }

    fn obj_create(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        self.inner.obj_create(name, args, ctx)
    }

    fn obj_get(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        self.inner.obj_get(obj, member, ctx)
    }

    fn obj_set(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        value: &Value,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        self.inner.obj_set(obj, member, value, ctx)
    }

    fn obj_call(
        &mut self,
        obj: &autoitv3_runtime::value::ObjRef,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        self.inner.obj_call(obj, member, args, ctx)
    }
}

/// Like [`host_platform_with`], but with fine-grained [`PlatformOptions`]:
/// per-area emulation routing (`force_emulated`) on top of the emulation
/// configuration. The presets' layer order is unchanged.
pub fn host_platform_with_options(options: PlatformOptions) -> Box<dyn Platform> {
    let PlatformOptions {
        emulation,
        force_emulated,
        assume_admin,
    } = options;
    // A simulated elevation is a property of the *user* the script sees, so it
    // travels with the emulation configuration (`AU3_WIN_ADMIN=0` picks a
    // standard user otherwise) and with the native layer's own answer.
    let emulation = if assume_admin {
        emulation.with_admin(true)
    } else {
        emulation
    };
    let emulated = emulation.is_enabled();
    let common = common_layer(&emulation);
    let declined: std::rc::Rc<[String]> = force_emulated
        .into_iter()
        .map(|n| n.to_ascii_lowercase())
        .collect();
    #[cfg(windows)]
    {
        let mut native = windows::WindowsPlatform::new().with_assumed_admin(assume_admin);
        if let Some(path) = emulation.module_path() {
            // The image under analysis answers `GetModuleHandleW(NULL)` so the
            // script reads its own resources (see `host_platform_with`).
            native = native.with_resource_module(path);
        }
        let native: Box<dyn Platform> = if declined.is_empty() {
            Box::new(native)
        } else {
            Box::new(FilteredPlatform {
                inner: native,
                declined: declined.clone(),
            })
        };
        let mut layers: Vec<Box<dyn Platform>> = vec![native, Box::new(common)];
        if emulated {
            layers.push(Box::new(emulation));
        }
        let name = if emulated {
            "windows+common+winemu"
        } else {
            "windows+common"
        };
        Box::new(CompositePlatform::new(name, layers))
    }
    #[cfg(not(windows))]
    {
        let _ = declined;
        let mut layers: Vec<Box<dyn Platform>> = Vec::new();
        if emulated {
            layers.push(Box::new(emulation));
        }
        layers.push(Box::new(common));
        layers.push(Box::new(LinuxPlatform::new()));
        let name = if emulated {
            "winemu+common+linux"
        } else {
            "common+linux"
        };
        Box::new(CompositePlatform::new(name, layers))
    }
}

/// A [`Runtime`] with the program loaded and this OS's platform stack installed.
///
/// Convenience for the common case; `Runtime::new()` alone has **no** platform,
/// so nothing outside the language builtins can be reached until one is
/// installed.
pub fn runtime_with_platform(prog: &autoitv3_ast::Program) -> Runtime {
    let mut rt = Runtime::with_program(prog);
    rt.set_platform(host_platform());
    rt
}
