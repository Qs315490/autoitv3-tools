//! Debug interfaces — the seam a debugger plugs into.
//!
//! The interpreter is built to be *instrumentable*: every `Stmt` carries a
//! `Span`, and [`crate::Runtime`] exposes structured call frames plus an
//! expression evaluator that works inside any frame. A debugger therefore only
//! needs to implement [`Debugger`] and register it.
//!
//! Nothing here performs I/O: the debugger decides what to do and returns a
//! [`DebugAction`], so the same interface serves an interactive console, a DAP
//! server, or an automated tracer. When the debugger *does* want to look at the
//! program it is handed a [`DebugHost`] — the live interpreter — so it can read
//! frames, evaluate expressions and edit breakpoints at the stop point.
//!
//! ## The shape of a stop
//!
//! The interpreter calls [`Debugger::on_statement`] before every statement,
//! whether or not anything is stopped, and the debugger answers with a
//! [`DebugAction`]. Answering [`DebugAction::Pause`] is what stops: the
//! interpreter then calls [`Debugger::on_stop`] *on the spot*, with the whole
//! Rust stack still live, and only carries on once that call returns. Stepping
//! is therefore the debugger's own bookkeeping — it remembers "stop at the next
//! statement" or "stop once we leave this frame" and picks the moment to say
//! `Pause` — rather than something the interpreter has to model.

use autoitv3_ast::span::Span;

use crate::error::RuntimeError;
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
    /// Number of times this breakpoint has been hit (counts hits that passed
    /// their condition, including ones swallowed by skip/every rules).
    pub hits: u64,
    /// Hits to skip before the breakpoint may fire again (an `ignore` budget).
    /// Each would-be hit that lands here consumes one skip without firing.
    pub skip_remaining: u64,
    /// Fire every *n*-th hit (1 = every hit). Combined with `skip_remaining`:
    /// skips are consumed first, then the every-*n* rule applies.
    pub every: u64,
    /// Whether reaching the breakpoint suspends execution. `false` makes it a
    /// logpoint: the `actions` run (and are reported) but the program carries
    /// straight on.
    pub stop: bool,
    /// Debugger **commands** run in order when the breakpoint fires — the
    /// "on-hit actions" (`print`, `eval`, `set`, `jmp`, ...). The interpreter
    /// hands the live host to [`Debugger::on_breakpoint_action`]; the debugger
    /// executes the commands there, so a `nostop` breakpoint becomes a
    /// logpoint.
    pub actions: Vec<String>,
}

impl Breakpoint {
    /// Create an enabled line breakpoint.
    pub fn at_line(id: u32, line: u32) -> Self {
        Self {
            id,
            line,
            column: None,
            enabled: true,
            condition: None,
            hits: 0,
            skip_remaining: 0,
            every: 1,
            stop: true,
            actions: Vec::new(),
        }
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
        self.add(line, None)
    }

    /// Add an enabled breakpoint on `line`, optionally guarded by an AutoIt
    /// expression that must evaluate to a true value for it to fire.
    pub fn add(&mut self, line: u32, condition: Option<String>) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let mut bp = Breakpoint::at_line(id, line);
        bp.condition = condition;
        self.items.push(bp);
        id
    }

    /// Add a breakpoint with the full spec (condition, hit rules, stop flag,
    /// on-hit actions), returning its id.
    pub fn add_full(
        &mut self,
        line: u32,
        condition: Option<String>,
        skip: u64,
        every: u64,
        stop: bool,
        actions: Vec<String>,
    ) -> u32 {
        let id = self.add(line, condition);
        if let Some(bp) = self.items.iter_mut().find(|b| b.id == id) {
            bp.skip_remaining = skip;
            bp.every = every.max(1);
            bp.stop = stop;
            bp.actions = actions;
        }
        id
    }

    /// Add `count` skips to a breakpoint (an `ignore` command): the next
    /// `count` would-be hits do not fire.
    pub fn ignore(&mut self, id: u32, count: u64) -> bool {
        match self.items.iter_mut().find(|b| b.id == id) {
            Some(b) => {
                b.skip_remaining += count;
                true
            }
            None => false,
        }
    }

    /// Set whether a breakpoint suspends execution on fire.
    pub fn set_stop(&mut self, id: u32, stop: bool) -> bool {
        match self.items.iter_mut().find(|b| b.id == id) {
            Some(b) => {
                b.stop = stop;
                true
            }
            None => false,
        }
    }

    /// Replace a breakpoint's on-hit actions.
    pub fn set_actions(&mut self, id: u32, actions: Vec<String>) -> bool {
        match self.items.iter_mut().find(|b| b.id == id) {
            Some(b) => {
                b.actions = actions;
                true
            }
            None => false,
        }
    }

    /// Look up a breakpoint by id.
    pub fn get(&self, id: u32) -> Option<&Breakpoint> {
        self.items.iter().find(|b| b.id == id)
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

    /// The first enabled breakpoint on `span`, without counting a hit.
    ///
    /// The interpreter asks this first so it can evaluate the breakpoint's
    /// condition; only a breakpoint that actually fires is counted.
    pub fn matching(&self, span: Span) -> Option<&Breakpoint> {
        self.items.iter().find(|b| b.matches(span))
    }

    /// Count one hit for the breakpoint with this id.
    pub fn record_hit(&mut self, id: u32) {
        if let Some(b) = self.items.iter_mut().find(|b| b.id == id) {
            b.hits += 1;
        }
    }

    /// Whether a breakpoint that just matched and passed its condition may
    /// fire now. Counts the hit (so `hits` includes skipped ones), consumes
    /// skips first, then applies the every-*n* rule to the hit number.
    pub fn should_fire(&mut self, id: u32) -> bool {
        let Some(b) = self.items.iter_mut().find(|b| b.id == id) else {
            return false;
        };
        b.hits += 1;
        if b.skip_remaining > 0 {
            b.skip_remaining -= 1;
            return false;
        }
        let every = b.every.max(1);
        b.hits % every == 0
    }

    /// The first enabled breakpoint matching `span`, if any. Increments its
    /// hit counter.
    pub fn hit(&mut self, span: Span) -> Option<Breakpoint> {
        let id = self.matching(span)?.id;
        self.record_hit(id);
        self.get(id).cloned()
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
    ///
    /// This fires for *every* statement, stopped or not: that is what lets a
    /// debugger implement stepping itself.
    fn on_statement(
        &mut self,
        span: Span,
        frame_depth: usize,
        host: &mut dyn DebugHost,
    ) -> DebugAction {
        let _ = (span, frame_depth, host);
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

    /// Called when the interpreter is about to invoke a builtin, host or
    /// platform function.
    ///
    /// Script-defined functions go through [`Debugger::on_call_enter`]; this is
    /// the counterpart for the calls the interpreter resolves itself
    /// (`GUICreate`, `String`, `DllCall`, ...), which have no script body and
    /// therefore no entry line. `name` is the spelling the script used.
    fn on_builtin_call(&mut self, name: &str) {
        let _ = name;
    }

    /// Called after a variable is written.
    fn on_variable_write(&mut self, name: &str, value: &Value) {
        let _ = (name, value);
    }

    /// Called when a statement fails with an uncaught runtime error.
    ///
    /// This fires where the error is *raised*, before the interpreter unwinds:
    /// the frame that raised it is still on the stack, so `host` can show the
    /// locals and the call stack that led there — a post-mortem. The error
    /// propagates as usual once this returns; there is no way to swallow it.
    fn on_error(&mut self, error: &RuntimeError, span: Option<Span>, host: &mut dyn DebugHost) {
        let _ = (error, span, host);
    }

    /// Called when a breakpoint with on-hit actions fires, before any
    /// stop-prompt. The actions are **debugger commands** (`print`, `eval`,
    /// `set`, ...), run by the debugger through `host` — so a `nostop`
    /// breakpoint acts as a logpoint, and a stopping one runs its commands
    /// before the prompt appears.
    fn on_breakpoint_action(&mut self, bp: &Breakpoint, host: &mut dyn DebugHost) {
        let _ = (bp, host);
    }

    /// Called when the interpreter stops.
    ///
    /// For [`StopReason::Breakpoint`], [`StopReason::Step`] and
    /// [`StopReason::Pause`] the interpreter is *suspended here*: the whole
    /// Rust stack is live, `host` can read and evaluate freely, and execution
    /// resumes when this returns. [`StopReason::Finished`] and
    /// [`StopReason::Error`] are end-of-run notifications — an interactive
    /// debugger should report those rather than prompt.
    fn on_stop(&mut self, reason: &StopReason, host: &mut dyn DebugHost) {
        let _ = (reason, host);
    }
}

/// The live interpreter, as seen by a stopped debugger.
///
/// Everything here is a read or an edit of the program *at the stop point*, so
/// an expression like `$x` resolves exactly as it would in the statement about
/// to run. Implemented by [`crate::Runtime`]; kept as a trait so a debugger can
/// be written — and tested — against a stub.
pub trait DebugHost {
    /// The call stack, outermost first. The last entry is the frame the
    /// interpreter is currently executing.
    fn frames(&self) -> Vec<FrameInfo>;

    /// Every global, sorted by name.
    fn globals(&self) -> Vec<(String, Value)>;

    /// Every function the program defines, sorted by name.
    fn function_names(&self) -> Vec<String>;

    /// Evaluate AutoIt source in the current frame. Assignments are allowed and
    /// take effect on the paused program, which is how `set` works.
    fn evaluate(&mut self, source: &str) -> Result<Value, RuntimeError>;

    /// Evaluate `source` as an *expression* in the current frame.
    ///
    /// Prefer this for anything a user typed as an expression: `$i = 5` is a
    /// comparison here and an assignment in [`DebugHost::evaluate`], and a
    /// `print` that quietly assigned would be a nasty surprise.
    fn evaluate_expression(&mut self, source: &str) -> Result<Value, RuntimeError>;

    /// The current breakpoints.
    fn breakpoints(&self) -> Vec<Breakpoint>;

    /// Add a breakpoint, optionally with a condition, returning its id.
    fn add_breakpoint(&mut self, line: u32, condition: Option<String>) -> u32;

    /// Add a breakpoint with the full spec (hit rules, stop flag, on-hit
    /// actions). Default: add a plain breakpoint and ignore the extras.
    fn add_breakpoint_full(
        &mut self,
        line: u32,
        condition: Option<String>,
        skip: u64,
        every: u64,
        stop: bool,
        actions: Vec<String>,
    ) -> u32 {
        let _ = (skip, every, stop, actions);
        self.add_breakpoint(line, condition)
    }

    /// Remove a breakpoint by id.
    fn remove_breakpoint(&mut self, id: u32) -> bool;

    /// Enable or disable a breakpoint by id.
    fn set_breakpoint_enabled(&mut self, id: u32, enabled: bool) -> bool;

    /// The first source line of the named function (its first statement, or
    /// the `Func` line when the body is empty) — what `break <func>` and
    /// `tbreak <func>` resolve to. `None` when no such function exists.
    fn function_entry_line(&self, name: &str) -> Option<u32> {
        let _ = name;
        None
    }

    /// Unconditionally transfer execution to `line` in the current frame:
    /// statements between here and there are skipped without running. The
    /// host validates the target; a target that is never reached simply has
    /// no effect. Default: not supported.
    fn jump_to(&mut self, line: u32) -> Result<(), RuntimeError> {
        let _ = line;
        Err(RuntimeError::Unsupported {
            what: "this host cannot jump".to_string(),
            span: None,
        })
    }

    /// Add skips to a breakpoint (`ignore`): the next `count` would-be hits
    /// do not fire. Default: no breakpoint table to edit.
    fn ignore_breakpoint(&mut self, id: u32, count: u64) -> bool {
        let _ = (id, count);
        false
    }

    /// Set whether a breakpoint suspends execution (`stop`/`nostop`).
    fn set_breakpoint_stop(&mut self, id: u32, stop: bool) -> bool {
        let _ = (id, stop);
        false
    }

    /// Replace a breakpoint's on-hit actions (`commands`).
    fn set_breakpoint_actions(&mut self, id: u32, actions: Vec<String>) -> bool {
        let _ = (id, actions);
        false
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
    fn on_statement(&mut self, span: Span, _depth: usize, _host: &mut dyn DebugHost) -> DebugAction {
        self.trace.push(span);
        DebugAction::Continue
    }

    fn on_call_enter(&mut self, name: &str, _args: &[Value]) {
        self.calls.push(name.to_string());
    }
}