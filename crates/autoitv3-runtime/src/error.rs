//! Runtime errors and control-flow signals.

use autoitv3_ast::span::Span;
use autoitv3_i18n::{msg, tr};
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
    /// `ContinueCase` — fall through to the next `Case` of the innermost
    /// `Select`/`Switch`, which consumes it. Anything else it reaches (a loop,
    /// a function boundary) passes it on rather than acting on it.
    ContinueCase,
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
    /// A declaration asked for more array elements than AutoIt allows.
    ArrayTooLarge { elements: i64, limit: i64, span: Option<Span> },
    /// Too many interpreter steps (runaway loop guard).
    ///
    /// The span is the statement that was running when the budget ran out, so
    /// a runaway loop is reported at the line it is spinning on instead of
    /// pointing at nothing.
    StepLimitExceeded { limit: u64, span: Option<Span> },
    /// Too many nested calls (runaway recursion guard).
    CallDepthExceeded { limit: usize },
    /// An error raised by a host/native function.
    Host { name: String, message: String },
    /// The debugger stopped the run on purpose (`quit`, or `run` to restart).
    ///
    /// This is a control signal rather than a script failure, so it is
    /// deliberately *not* offered to [`crate::debug::Debugger::on_error`].
    Aborted,
}

impl RuntimeError {
    /// A short, human-readable description.
    pub fn message(&self) -> String {
        match self {
            RuntimeError::Unsupported { what, .. } => {
                msg!("unsupported construct: {what}", what = what)
            }
            RuntimeError::Type { expected, got, .. } => {
                msg!("type error: expected {expected}, got {got}", expected = expected, got = got)
            }
            RuntimeError::UndefinedVariable { name, .. } => {
                msg!("undefined variable: {name}", name = name)
            }
            RuntimeError::UndefinedFunction { name, .. } => {
                msg!("undefined function: {name}", name = name)
            }
            RuntimeError::IndexOutOfBounds { index, len, .. } => {
                msg!("index {index} out of bounds (len {len})", index = index, len = len)
            }
            RuntimeError::ArrayTooLarge { elements, limit, .. } => msg!(
                "array of {elements} elements is past the {limit} AutoIt allows \
                 (VAR_SUBSCRIPT_ELEMENTS)",
                elements = elements,
                limit = limit
            ),
            RuntimeError::StepLimitExceeded { limit, .. } => {
                msg!("step limit exceeded ({limit})", limit = limit)
            }
            RuntimeError::CallDepthExceeded { limit } => {
                msg!("call depth exceeded ({limit})", limit = limit)
            }
            RuntimeError::Host { name, message } => {
                msg!("host function {name}: {message}", name = name, message = message)
            }
            RuntimeError::Aborted => tr("aborted by the debugger").to_string(),
        }
    }

    /// The source span this error points at, when known.
    pub fn span(&self) -> Option<Span> {
        match self {
            RuntimeError::Unsupported { span, .. }
            | RuntimeError::Type { span, .. }
            | RuntimeError::UndefinedVariable { span, .. }
            | RuntimeError::UndefinedFunction { span, .. }
            | RuntimeError::IndexOutOfBounds { span, .. }
            | RuntimeError::ArrayTooLarge { span, .. }
            | RuntimeError::StepLimitExceeded { span, .. } => *span,
            _ => None,
        }
    }

    /// Helper for building an "unsupported" error at a span.
    pub fn unsupported(what: impl Into<String>, span: Span) -> Self {
        RuntimeError::Unsupported { what: what.into(), span: Some(span) }
    }
}

impl fmt::Display for RuntimeError {
    /// The message, with the source position when the error knows one — a
    /// bare "index out of bounds" is hard to act on in a 23k-line script.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span() {
            Some(s) => write!(
                f,
                "{}",
                msg!(
                    "{message} (at {line}:{col})",
                    message = self.message(),
                    line = s.start.line,
                    col = s.start.col
                )
            ),
            None => write!(f, "{}", self.message()),
        }
    }
}

impl std::error::Error for RuntimeError {}