//! Platform layer — where OS-specific builtins live.
//!
//! The interpreter core, the value model and the portable builtin subset
//! ([`crate::builtins`]) are platform-neutral. AutoIt's own function library,
//! however, is largely a thin wrapper over the Windows API: the registry, COM,
//! `DllCall`, GUICtrl*, process/window management, and so on.
//!
//! Rather than let those leak into the core, each operating system gets an
//! implementation of [`Platform`]:
//!
//! | module | selected for | contents |
//! |---|---|---|
//! | [`generic`] | Linux (and any non-Windows target) | nothing OS-specific — AutoIt is a Windows tool, so the honest answer is "not provided" |
//! | [`windows`] | Windows | the extension point for registry/COM/DllCall/GUI |
//!
//! [`host_platform`] picks the right one at compile time; a build only ever
//! compiles its own platform module.
//!
//! Lookup order for a call the interpreter cannot resolve itself is
//! **builtins → host → platform**, so an embedding application's [`Host`] can
//! always override the platform default.
//!
//! [`Host`]: crate::host::Host

pub mod generic;

#[cfg(windows)]
pub mod windows;

pub use generic::GenericPlatform;

#[cfg(windows)]
pub use windows::WindowsPlatform;

use crate::error::RuntimeError;
use crate::host::HostContext;
use crate::value::Value;

/// The platform that backs the interpreter on this build target.
pub fn host_platform() -> Box<dyn Platform> {
    #[cfg(windows)]
    {
        Box::new(WindowsPlatform::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(GenericPlatform::new())
    }
}

/// An operating-system integration providing AutoIt builtins that cannot be
/// implemented portably.
///
/// Every method has a usable default, so a platform only declares what it
/// actually supports.
pub trait Platform {
    /// A short identifier, e.g. `linux-generic` or `windows`.
    fn name(&self) -> &'static str;

    /// Whether this platform can service `name`.
    ///
    /// Used by `IsFunc`-style checks; the default implementation is
    /// pessimistic, so implementations that can answer cheaply should
    /// override it.
    fn provides(&self, name: &str) -> bool {
        let _ = name;
        false
    }

    /// Service a call.
    ///
    /// `Ok(None)` means "this platform does not provide `name`", which lets the
    /// interpreter report an undefined function.
    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let _ = (name, args, ctx);
        Ok(None)
    }
}