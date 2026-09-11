//! Windows platform implementation — the extension point for AutoIt's
//! Windows-only library.
//!
//! This module is compiled only on Windows targets (see
//! [`crate::host_platform`]). It is currently a **scaffold**: the
//! registration table and dispatch are in place, but no function is
//! implemented yet, so every call answers `Ok(None)` and the interpreter
//! reports an undefined function exactly as it does on Linux.
//!
//! The functions below are the intended inhabitants, grouped by the AutoIt
//! area they come from:
//!
//! * **registry** — `RegRead`, `RegWrite`, `RegDelete`, `RegEnumKey`,
//!   `RegEnumVal`
//! * **COM** — `ObjCreate`, `ObjGet`, `ObjEvent`, `IsObj`, and the
//!   `.Member` / `.Method()` access that [`autoitv3_runtime::Runtime`] currently reports
//!   as needing a platform host
//! * **native calls** — `DllCall`, `DllCallAddress`, `DllStruct*`
//! * **GUI** — `GUICreate`, `GUICtrl*`, `GUIGetMsg`, `GUISetState`
//! * **process / window** — `Run`, `Process*`, `Win*`, `Control*`
//! * **system** — `ClipGet`/`ClipPut`, `Env*`, `DriveGet*`, `Shutdown`
//!
//! Adding one means writing a function and listing it in
//! [`WindowsPlatform::provides`] / [`WindowsPlatform::call`]; nothing outside
//! this module needs to change.

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use autoitv3_runtime::platform::Platform;

/// The platform used when the target OS is Windows.
#[derive(Debug, Default)]
pub struct WindowsPlatform {
    _private: (),
}

impl WindowsPlatform {
    /// Create the Windows platform.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Platform for WindowsPlatform {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn provides(&self, _name: &str) -> bool {
        // No Windows-only builtin is implemented yet; see the module docs for
        // the planned set.
        false
    }

    fn call(
        &mut self,
        _name: &str,
        _args: Vec<Value>,
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        Ok(None)
    }
}