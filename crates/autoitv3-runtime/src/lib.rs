//! `autoitv3-runtime` — an AutoIt v3 runtime: value model, interpreter, and the
//! interfaces a full runtime and a debugger plug into.
//!
//! # Why this crate exists
//!
//! Deobfuscating a script's payload needs *execution*: the obfuscator stores
//! its function table and its string table in arrays that are built by running
//! generated helper functions. Pure AST rewriting can resolve the function
//! table (it is built entirely from array literals), but the string table
//! depends on string manipulation, `Execute`, maps and loops — so the
//! deobfuscator needs a real interpreter.
//!
//! # Layout
//!
//! | module | role |
//! |---|---|
//! | [`value`] | runtime values (`Int`, `Str`, `Array`, `Map`, ...) and AutoIt coercion rules |
//! | [`interp`] | the [`Runtime`]: load a program, call functions, evaluate expressions |
//! | [`builtins`] | the implemented subset of AutoIt's function library |
//! | [`regexp`] | `StringRegExp*` on a pure-Rust engine (platform-independent) |
//! | [`profile`] | [`ExecutionProfile`] — faithful AutoIt semantics vs. fast, reproducible deobfuscation |
//! | [`host`] | [`host::Host`] — how a *complete* runtime plugs native functions in |
//! | [`platform`] | [`platform::Platform`] — OS-specific builtins (Linux generic / Windows) |
//! | [`debug`] | [`debug::Debugger`], breakpoints and call frames — the debug module's seam |
//! | [`error`] | [`error::RuntimeError`] and control-flow signals |
//!
//! # Example
//!
//! ```
//! use autoitv3_runtime::{Runtime, Value};
//!
//! let prog = autoitv3_ast::parse("Func Add($a, $b)\n    Return $a + $b\nEndFunc\n").unwrap();
//! let mut rt = Runtime::with_program(&prog);
//! let r = rt.call_function("Add", vec![Value::Int(2), Value::Int(3)]).unwrap();
//! assert!(matches!(r, Value::Int(5)));
//! ```

pub mod builtins;
pub mod debug;
pub mod error;
pub mod host;
pub mod interp;
pub mod platform;
pub mod profile;
pub mod regexp;
pub mod value;
pub mod vocab;

pub use debug::{
    Breakpoint, Breakpoints, DebugAction, Debugger, FrameInfo, StopReason, TracingDebugger,
};
pub use error::{Flow, RuntimeError};
pub use host::{Host, HostContext, NativeFn, NativeHost};
pub use interp::{is_constant_expr, Runtime};
pub use platform::Platform;
pub use profile::{EffectKind, EffectOverrides, EffectPolicy, ExecutionProfile, RandomPolicy, SleepPolicy};
pub use value::{ArrayRef, MapRef, Value};