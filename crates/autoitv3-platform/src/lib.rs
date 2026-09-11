//! `autoitv3-platform` — operating-system integrations for `autoitv3-runtime`.
//!
//! AutoIt v3 is a Windows automation language: a good part of its function
//! library is a thin wrapper over the Win32 API. The interpreter core, the
//! value model and the language-level builtins therefore stay platform-neutral,
//! and the library functions are layered here:
//!
//! | layer | module | installed on | contents |
//! |---|---|---|---|
//! | portable | [`portable`] | **every** platform | file/directory I/O, environment, math, timers, console — the parts AutoIt does the same way everywhere |
//! | emulation | [`winemu`] | non-Windows | Windows-flavoured identity, paths, `DllStruct*`/`DllCall`, registry, clipboard and drives, so a Windows-targeted script keeps running off Windows |
//! | system | [`linux`] / [`windows`] | one OS each | what genuinely differs: process queries on Linux; registry, COM, `DllCall`, GUI on Windows |
//!
//! [`host_platform`] builds the right stack for the target at compile time; a
//! build only ever compiles its own system layer. The layers are tried in order
//! by [`CompositePlatform`], so the portable functions are available on Windows
//! too and a system layer only has to add what is actually system-specific.
//!
//! On non-Windows targets the stack is **emulation → portable → linux**: the
//! [`WindowsEmulation`] layer answers first (it deliberately shadows the
//! portable directory macros), and anything it does not know falls through to
//! the host. Set `AU3_WIN_EMU=0` — or install a disabled [`WindowsEmulation`]
//! with [`host_platform_with`] — for the plain `portable+linux` stack, where
//! Windows-only calls stay undefined functions.
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
//! assert!(rt.platform_name().contains("portable"));
//! ```
//!
//! Lookup order for a call the interpreter cannot resolve itself is
//! **builtins → host → platform**, so an embedding application's
//! [`Host`](autoitv3_runtime::host::Host) can always override a
//! platform-provided function.

pub mod linux;
pub mod portable;
pub mod winemu;

#[cfg(windows)]
pub mod windows;

pub use linux::LinuxPlatform;
pub use portable::PortablePlatform;
pub use winemu::{
    find_resource_module, has_staged_resources, resource_search_dirs, FileRegistry, MemoryRegistry,
    PeImage, RegistryData, RegistryStore, Selector, WindowsArch, WindowsEmulation, WindowsVersion,
};

#[cfg(windows)]
pub use windows::WindowsPlatform;

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::value::Value;
use autoitv3_runtime::Runtime;

/// Several platforms tried in order.
///
/// This is how the portable layer and the system layer compose: the first layer
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

    /// Ask each layer in order, so the portable layer can answer the
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
            if let Some(v) = layer.call(name, args.clone(), ctx)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }
}

/// The platform stack for the operating system this build targets.
///
/// On a Windows build this is `portable+windows`. On every other target it is
/// `winemu+portable+linux`: the [`WindowsEmulation`] layer is configured from
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
/// The layer is installed first so its macros win over the portable ones; a
/// disabled emulation is left out entirely. Windows builds ignore `emulation`
/// and use the real [`WindowsPlatform`].
pub fn host_platform_with(emulation: WindowsEmulation) -> Box<dyn Platform> {
    #[cfg(windows)]
    {
        let _ = emulation;
        Box::new(CompositePlatform::new(
            "portable+windows",
            vec![
                Box::new(PortablePlatform::new()),
                Box::new(WindowsPlatform::new()),
            ],
        ))
    }
    #[cfg(not(windows))]
    {
        let emulated = emulation.is_enabled();
        let mut layers: Vec<Box<dyn Platform>> = Vec::new();
        if emulated {
            layers.push(Box::new(emulation));
        }
        layers.push(Box::new(PortablePlatform::new()));
        layers.push(Box::new(LinuxPlatform::new()));
        let name = if emulated {
            "winemu+portable+linux"
        } else {
            "portable+linux"
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
