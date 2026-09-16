//! Host interface — how a *complete* runtime plugs native functionality in.
//!
//! The small interpreter in this crate implements a self-contained subset of
//! AutoIt's builtin library (enough to evaluate the obfuscator's array/string
//! tables). A full runtime needs the real thing: Win32 APIs, COM, GUICtrl*,
//! DllCall, file I/O, and so on.
//!
//! Rather than growing the builtin table forever, embedding applications
//! implement [`Host`] and register it on the [`crate::Runtime`]. When the
//! interpreter cannot resolve a call against its own builtins it asks the host,
//! which keeps the interpreter core independent of any platform.

use crate::error::RuntimeError;
use crate::profile::{EffectKind, ExecutionProfile};
use crate::value::Value;

/// A native function callable from interpreted AutoIt code.
pub type NativeFn = dyn Fn(&mut dyn HostContext, Vec<Value>) -> Result<Value, RuntimeError>;

/// Services the interpreter exposes back to native functions.
///
/// This is the seam a full runtime and the debugger use to reach interpreter
/// state without depending on its internals.
pub trait HostContext {
    /// Read a global variable.
    fn get_global(&self, name: &str) -> Option<Value>;
    /// Write a global variable.
    fn set_global(&mut self, name: &str, value: Value);
    /// The current `@error` value.
    fn error(&self) -> i64;
    /// Set `@error` / `@extended`, mirroring `SetError()`.
    fn set_error(&mut self, error: i64, extended: i64);
    /// The execution profile in force, so a platform can honour it (skipping
    /// `Sleep`, seeding `Random`, refusing writes).
    fn profile(&self) -> &ExecutionProfile;
    /// The value of an AutoIt option, as the script last set it.
    ///
    /// `Opt`/`AutoItSetOption` is interpreter state — the runtime owns the table
    /// and answers the return value — but some options only mean something to a
    /// platform: the GUI's `GUIOnEventMode`, a tray's `TrayOnEventMode`. Those
    /// read it here. `None` means the script has not set it, so the platform
    /// applies the documented default.
    fn option(&self, name: &str) -> Option<Value> {
        let _ = name;
        None
    }
    /// The effective decision for one class of external effect: the profile's
    /// per-kind override when set, otherwise its base [`EffectPolicy`].
    ///
    /// Platform layers gate every side effect through this instead of a bare
    /// "are writes allowed", so an embedder (or the CLI's `--allow`/`--deny`)
    /// can fine-tune individual effects — e.g. permit registry writes in a
    /// deobfuscation run, or forbid `Shutdown` in a faithful one.
    fn effect_allowed(&self, kind: EffectKind) -> bool {
        let allowed = self.profile().effect_allowed(kind);
        if !allowed {
            note_refused_effect(kind);
        }
        allowed
    }
}

/// Report a refused side effect on stderr, once per kind per process.
///
/// A refusal is otherwise indistinguishable from the machine refusing it: the
/// call returns its failure value and sets `@error = 1`, which is exactly what
/// a real permission problem looks like (`DirCreate` returning 0 cost one user
/// an afternoon of chasing ACLs). The note names the profile's decision and how
/// to overrule it, and fires only when a script actually tried.
fn note_refused_effect(kind: EffectKind) {
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<EffectKind>>> =
        std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let seen = seen.get_or_insert_with(std::collections::HashSet::new);
    if seen.insert(kind) {
        eprintln!(
            "{}",
            autoitv3_i18n::msg!(
                "note: a {kind} side effect was refused by the execution profile — \
                 use --allow {kind} to permit this kind, or --faithful to let the script \
                 do its side effects for real",
                kind = kind.name()
            )
        );
    }
}

/// A pluggable provider of native functions.
///
/// Implement this to back the interpreter with a real AutoIt-compatible
/// runtime. Returning `Ok(None)` from [`Host::call`] means "I do not provide
/// this function", letting the interpreter report an undefined function.
pub trait Host {
    /// Call `name` with `args`. `Ok(None)` means the host does not provide it.
    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError>;

    /// Whether this host provides `name` (used for `IsFunc` and pre-checks).
    fn provides(&self, name: &str) -> bool {
        let _ = name;
        false
    }
}

/// A `Host` backed by a plain name → closure table.
///
/// Useful for tests, for the deobfuscator (which registers the handful of
/// builtins it cares about), and as the building block for a bigger host.
#[derive(Default)]
pub struct NativeHost {
    funcs: Vec<(String, Box<NativeFn>)>,
}

impl NativeHost {
    /// Create an empty host.
    pub fn new() -> Self {
        Self { funcs: Vec::new() }
    }

    /// Register a native function under `name`.
    pub fn register<F>(&mut self, name: impl Into<String>, f: F) -> &mut Self
    where
        F: Fn(&mut dyn HostContext, Vec<Value>) -> Result<Value, RuntimeError> + 'static,
    {
        self.funcs.push((name.into(), Box::new(f)));
        self
    }
}

impl Host for NativeHost {
    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        // Case-insensitive lookup, matching AutoIt's function-name rules.
        for (n, f) in &mut self.funcs {
            if n.eq_ignore_ascii_case(name) {
                return f(ctx, args).map(Some);
            }
        }
        Ok(None)
    }

    fn provides(&self, name: &str) -> bool {
        self.funcs.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
}