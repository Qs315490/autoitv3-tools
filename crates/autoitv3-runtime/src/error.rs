//! Runtime errors and control-flow signals.

use autoitv3_ast::span::Span;
use std::fmt;

/// Control flow produced by executing a statement.
///
/// The interpreter threads this through statement execution so that `Return`,
/// loop control and `Exit` unwind correctly.
#[derive(Debug, Clone)]
pub enum Flow {
    /// Fall through to the next statement.
    Normal,
    /// `Return [expr]` — unwind to the end of the current function.
    Return(crate::value::Value),
    /// `ExitLoop n` — break out of `n` enclosing loops.
    Break(usize),
    /// `ContinueLoop n` — continue the `n`-th enclosing loop.
    Continue(usize),
    /// `Exit [code]` — terminate the whole script.
    Exit(i32),
}

impl Flow {
    /// True for the fall-through case.
    pub fn is_normal(&self) -> bool {
        matches!(self, Flow::Normal)
    }
}

/// An error raised while interpreting.
#[derive(Debug, Clone)]
pub enum RuntimeError {
    /// The interpreter does not implement this construct yet.
    Unsupported { what: String, span: Option<Span> },
    /// A type error (e.g. indexing a non-array).
    Type { expected: &'static str, got: String, span: Option<Span> },
    /// Reading a variable that was never assigned.
    UndefinedVariable { name: String, span: Option<Span> },
    /// Calling a function that does not exist.
    UndefinedFunction { name: String, span: Option<Span> },
    /// An array/string index outside its bounds.
    IndexOutOfBounds { index: i64, len: usize, span: Option<Span> },
    /// Too many interpreter steps (runaway loop guard).
    StepLimitExceeded { limit: u64 },
    /// Too many nested calls (runaway recursion guard).
    CallDepthExceeded { limit: usize },
    /// An error raised by a host/native function.
    Host { name: String, message: String },
}

impl RuntimeError {
    /// A short, human-readable description.
    pub fn message(&self) -> String {
        match self {
            RuntimeError::Unsupported { what, .. } => format!("unsupported construct: {what}"),
            RuntimeError::Type { expected, got, .. } => {
                format!("type error: expected {expected}, got {got}")
            }
            RuntimeError::UndefinedVariable { name, .. } => format!("undefined variable: {name}"),
            RuntimeError::UndefinedFunction { name, .. } => format!("undefined function: {name}"),
            RuntimeError::IndexOutOfBounds { index, len, .. } => {
                format!("index {index} out of bounds (len {len})")
            }
            RuntimeError::StepLimitExceeded { limit } => {
                format!("step limit exceeded ({limit})")
            }
            RuntimeError::CallDepthExceeded { limit } => {
                format!("call depth exceeded ({limit})")
            }
            RuntimeError::Host { name, message } => format!("host function {name}: {message}"),
        }
    }

    /// The source span this error points at, when known.
    pub fn span(&self) -> Option<Span> {
        match self {
            RuntimeError::Unsupported { span, .. }
            | RuntimeError::Type { span, .. }
            | RuntimeError::UndefinedVariable { span, .. }
            | RuntimeError::UndefinedFunction { span, .. }
            | RuntimeError::IndexOutOfBounds { span, .. } => *span,
            _ => None,
        }
    }

    /// Helper for building an "unsupported" error at a span.
    pub fn unsupported(what: impl Into<String>, span: Span) -> Self {
        RuntimeError::Unsupported { what: what.into(), span: Some(span) }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for RuntimeError {}