//! Portable platform implementation (Linux and any other non-Windows target).
//!
//! AutoIt v3 is a Windows automation language: the great majority of its
//! library is a wrapper over the Win32 API. On Linux there is therefore no
//! meaningful implementation to provide, and this module deliberately stays
//! empty rather than inventing behaviour.
//!
//! It still exists as a real [`Platform`], so:
//!
//! * the interpreter has a stable, non-optional platform to call into;
//! * a Linux build has a well-defined place to grow portable functionality
//!   (for example file-system or process helpers) without touching the core;
//! * behaviour is identical whatever the host OS for everything that *is*
//!   portable, because the portable subset lives in [`crate::builtins`].
//!
//! Every call answers `Ok(None)` — "not provided here" — which the interpreter
//! turns into an undefined-function error.

use super::Platform;
use crate::error::RuntimeError;
use crate::host::HostContext;
use crate::value::Value;

/// The platform used when the target OS is not Windows.
#[derive(Debug, Default)]
pub struct GenericPlatform {
    _private: (),
}

impl GenericPlatform {
    /// Create the portable platform.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Platform for GenericPlatform {
    fn name(&self) -> &'static str {
        "linux-generic"
    }

    fn provides(&self, _name: &str) -> bool {
        // Nothing OS-specific is available off Windows.
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