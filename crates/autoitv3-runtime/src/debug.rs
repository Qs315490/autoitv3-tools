//! Debug interfaces — the seam a future debugger plugs into.
//!
//! The interpreter is built to be *instrumentable*: every `Stmt` carries a
//! `Span`, and [`crate::Runtime`] exposes structured call frames plus an
//! expression evaluator that works inside any frame. A debugger therefore only
//! needs to implement [`Debugger`] and register it.
//!
//! Nothing here performs I/O or blocks: the debugger decides what to do and
//! returns a [`DebugAction`], so the same interface serves an interactive
//! console, a DAP server, or an automated tracer.

use autoitv3_ast::span::Span;

use crate::value::Value;

/// A breakpoint, identified by source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    /// Stable identifier assigned by the debugger.
    pub id: u32,
    /// 1-based source line the breakpoint sits on.
    pub line: u32,
    /// Optional 1-based column; `None` matches the whole line.
    pub column: Option<u32>,
    /// Disabled breakpoints are retained but never fire.
    pub enabled: bool,
    /// Optional AutoIt condition expression; the breakpoint fires only when it
    /// evaluates to a true value.
    pub condition: Option<String>,
    /// Number of times this breakpoint has been hit.
    pub hits: u64,
}

impl Breakpoint {
    /// Create an enabled line breakpoint.
    pub fn at_line(id: u32, line: u32) -> Self {
        Self { id, line, column: None, enabled: true, condition: None, hits: 0 }
    }

    /// True when `span` starts on this breakpoint's position.
    pub fn matches(&self, span: Span) -> bool {
        if !self.enabled || span.start.line != self.line {
            return false;
        }
        match self.column {
            Some(c) => span.start.col == c,
            None => true,
        }
    }
}

/// A collection of breakpoints with efficient span lookup.
#[derive(Debug, Default, Clone)]
pub struct Breakpoints {
    items: Vec<Breakpoint>,
    next_id: u32,
}

impl Breakpoints {
    /// Create an empty set.
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: 1 }
    }

    /// Add an enabled breakpoint on `line`, returning its id.
    pub fn add_line(&mut self, line: u32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Breakpoint::at_line(id, line));
        id
    }

    /// Remove a breakpoint by id. Returns whether it existed.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.id != id);
        self.items.len() != before
    }

    /// Enable or disable a breakpoint by id.
    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> bool {
        match self.items.iter_mut().find(|b| b.id == id) {
            Some(b) => {
                b.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// All breakpoints.
    pub fn items(&self) -> &[Breakpoint] {
        &self.items
    }

    /// The first enabled breakpoint matching `span`, if any. Increments its
    /// hit counter.
    pub fn hit(&mut self, span: Span) -> Option<&Breakpoint> {
        if let Some(b) = self.items.iter_mut().find(|b| b.matches(span)) {
            b.hits += 1;
            // Re-borrow immutably for the return value.
            let id = b.id;
            return self.items.iter().find(|b| b.id == id);
        }
        None
    }
}

/// Why the interpreter stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// A breakpoint was hit.
    Breakpoint { id: u32, line: u32 },
    /// A single step completed.
    Step,
    /// The user paused execution.
    Pause,
    /// Execution finished normally.
    Finished,
    /// Execution stopped because of an error.
    Error,
}

/// One activation record, as seen by a debugger.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    /// The function name, or `None` for top-level (script) code.
    pub function: Option<String>,
    /// Depth, starting at 0 for top-level code.
    pub depth: usize,
    /// Source span currently executing in this frame (if any).
    pub span: Option<Span>,
    /// Local variables visible in this frame.
    pub locals: Vec<(String, Value)>,
}

/// Callbacks the interpreter invokes while running.
///
/// Every method has a default no-op body, so an implementation only overrides
/// what it needs.
pub trait Debugger {
    /// Called before a statement executes. Return an action to control flow.
    fn on_statement(&mut self, span: Span, frame_depth: usize) -> DebugAction {
        let _ = (span, frame_depth);
        DebugAction::Continue
    }

    /// Called when a function is entered.
    fn on_call_enter(&mut self, name: &str, args: &[Value]) {
        let _ = (name, args);
    }

    /// Called when a function returns (including on the error path).
    fn on_call_exit(&mut self, name: &str, result: Option<&Value>) {
        let _ = (name, result);
    }

    /// Called after a variable is written.
    fn on_variable_write(&mut self, name: &str, value: &Value) {
        let _ = (name, value);
    }

    /// Called when the interpreter stops for any reason.
    fn on_stop(&mut self, reason: &StopReason) {
        let _ = reason;
    }
}

/// What the interpreter should do after a debug callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAction {
    /// Keep running.
    Continue,
    /// Suspend execution and surface control to the debugger.
    Pause,
    /// Abort execution.
    Abort,
}

/// A debugger that records every statement span it sees.
///
/// This is the reference implementation used by the tests, and a useful
/// building block for coverage/tracing tools.
#[derive(Debug, Default)]
pub struct TracingDebugger {
    /// Spans observed, in execution order.
    pub trace: Vec<Span>,
    /// Function names entered, in order.
    pub calls: Vec<String>,
}

impl Debugger for TracingDebugger {
    fn on_statement(&mut self, span: Span, _depth: usize) -> DebugAction {
        self.trace.push(span);
        DebugAction::Continue
    }

    fn on_call_enter(&mut self, name: &str, _args: &[Value]) {
        self.calls.push(name.to_string());
    }
}