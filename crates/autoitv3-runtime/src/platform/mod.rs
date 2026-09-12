//! Platform layer — where OS-specific builtins live.
//!
//! The interpreter core, the value model and the portable builtin subset
//! ([`crate::builtins`]) are platform-neutral. AutoIt's own function library,
//! however, is largely a thin wrapper over the Windows API: the registry, COM,
//! `DllCall`, GUICtrl*, process/window management, and so on.
//!
//! Rather than let those leak into the core, each operating system gets an
//! implementation of [`Platform`]. The concrete implementations (`generic` for Linux, `windows` for Windows)
//! and the [`host_platform`] factory live in `autoitv3-platform`.
//!
//! [`host_platform`]: https://docs.rs/autoitv3-platform
//!
//! Lookup order for a call the interpreter cannot resolve itself is
//! **builtins → host → platform**, so an embedding application's [`Host`] can
//! always override the platform default.
//!
//! [`Host`]: crate::host::Host

use crate::error::RuntimeError;
use crate::host::HostContext;
use crate::value::Value;

/// An operating-system integration providing AutoIt builtins that cannot be
/// implemented portably.
///
/// The trait lives here because it is the seam [`crate::Runtime`] calls into;
/// the implementations live in the sibling crate `autoitv3-platform`, which
/// depends on this one. That keeps the dependency direction one-way: the
/// interpreter core never names a concrete operating system.
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

    /// Value of an AutoIt macro this platform can answer, e.g. `@TempDir`,
    /// `@OSVersion`, `@ComputerName`.
    ///
    /// The interpreter resolves the universal macros itself (`@error`,
    /// `@CRLF`, `@ScriptLineNumber`, ...) and only asks the platform for the
    /// ones that depend on the environment. `None` means "not provided"; the
    /// macro then evaluates to `Null` rather than a fabricated value.
    fn macro_value(&self, name: &str) -> Option<Value> {
        let _ = name;
        None
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

    // ----- object (COM) seams -----

    /// Create an object for `name` (a ProgID, typically) with `args`.
    /// `Ok(None)` means this platform does not create objects.
    fn obj_create(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let _ = (name, args, ctx);
        Ok(None)
    }

    /// Read a property `member` of a platform object.
    fn obj_get(
        &mut self,
        obj: &crate::value::ObjRef,
        member: &str,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let _ = (obj, member, ctx);
        Ok(None)
    }

    /// Assign `value` to property `member` of a platform object.
    fn obj_set(
        &mut self,
        obj: &crate::value::ObjRef,
        member: &str,
        value: &Value,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let _ = (obj, member, value, ctx);
        Ok(None)
    }

    /// Call method `member` of a platform object with `args`.
    fn obj_call(
        &mut self,
        obj: &crate::value::ObjRef,
        member: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let _ = (obj, member, args, ctx);
        Ok(None)
    }
}
