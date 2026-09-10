//! `autoitv3-platform` — operating-system integrations for `autoitv3-runtime`.
//!
//! AutoIt v3 is a Windows automation language: most of its function library is
//! a thin wrapper over the Win32 API. The interpreter core, the value model and
//! the portable builtin subset therefore stay platform-neutral, and each
//! operating system gets an implementation of
//! [`Platform`](autoitv3_runtime::platform::Platform) here.
//!
//! | module | selected for | contents |
//! |---|---|---|
//! | [`generic`] | Linux (and any non-Windows target) | nothing OS-specific — AutoIt is a Windows tool, so the honest answer is "not provided" |
//! | [`windows`] | Windows | the extension point for registry/COM/`DllCall`/GUI |
//!
//! [`host_platform`] picks the right one at compile time; a build only ever
//! compiles its own platform module.
//!
//! # Layering
//!
//! The [`Platform`](autoitv3_runtime::platform::Platform) *trait* lives in
//! `autoitv3-runtime` (it is the seam the interpreter calls into), while the
//! *implementations* live here. That keeps the dependency direction one-way —
//! this crate depends on the runtime, never the reverse — so the core has no
//! knowledge of any particular operating system.
//!
//! # Using it
//!
//! ```
//! let prog = autoitv3_ast::parse("Func F()\n    Return 1\nEndFunc\n").unwrap();
//! let mut rt = autoitv3_platform::runtime_with_platform(&prog);
//! assert_eq!(rt.platform_name(), if cfg!(windows) { "windows" } else { "linux-generic" });
//! ```
//!
//! Lookup order for a call the interpreter cannot resolve itself is
//! **builtins → host → platform**, so an embedding application's
//! [`Host`](autoitv3_runtime::host::Host) can always override a
//! platform-provided function.

pub mod generic;

#[cfg(windows)]
pub mod windows;

pub use generic::GenericPlatform;

#[cfg(windows)]
pub use windows::WindowsPlatform;

use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::Runtime;

/// The platform implementation for the operating system this build targets.
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

/// A [`Runtime`] with the program loaded and this OS's platform installed.
///
/// Convenience for the common case; `Runtime::new()` alone has **no** platform,
/// so nothing OS-specific can be reached until one is installed.
pub fn runtime_with_platform(prog: &autoitv3_ast::Program) -> Runtime {
    let mut rt = Runtime::with_program(prog);
    rt.set_platform(host_platform());
    rt
}