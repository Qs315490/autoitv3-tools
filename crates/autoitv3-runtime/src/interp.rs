//! The interpreter core.
//!
//! [`Runtime`] loads a parsed [`Program`](autoitv3_ast::ast::Program) and can
//! call into it. It is deliberately small but semantically careful about the
//! parts of AutoIt the obfuscator leans on:
//!
//! * `ByRef` parameters (copy-in/copy-out plus shared array storage)
//! * `ReDim` resizing an array in place
//! * `For ... To ... Step` and `For ... In`
//! * compound assignment (`+=`, `&=`, ...)
//! * `@error` / `@extended`
//!
//! It also exposes the seams a debugger and a full runtime need — see
//! [`crate::debug`] and [`crate::host`].

use std::collections::HashMap;

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;

use crate::builtins;
use crate::debug::{
    Breakpoint, Breakpoints, DebugAction, DebugHost, Debugger, FrameInfo, StopReason,
};
use crate::error::{Flow, RuntimeError};
use crate::host::{Host, HostContext};
use crate::platform::Platform;
use crate::profile::ExecutionProfile;
use crate::value::{MapKey, Value};

/// Default runaway-loop guard.
///
/// The single budget every entry point starts from: a bare [`Runtime`] and the
/// deobfuscation passes (which evaluate the obfuscator's own table builders) all
/// use this value, so there is one default to reason about. `0` disables the
/// check; callers override it with [`Runtime::set_max_steps`].
pub const DEFAULT_MAX_STEPS: u64 = 20_000_000;
/// Default recursion guard.
pub const DEFAULT_MAX_DEPTH: usize = 256;

/// Upper bound on callback invocations drained after a single platform call,
/// so a scripted enumerator cannot flood the run.
const MAX_PENDING_CALLBACKS: usize = 10_000;

/// One activation record.
struct Frame {
    vars: HashMap<String, Value>,
    function: Option<String>,
    span: Option<Span>,
    /// How many arguments the caller passed, for `@NumParams`.
    arg_count: usize,
}

/// The AutoIt interpreter.
pub struct Runtime {
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    /// Lower-cased function name -> definition.
    funcs: HashMap<String, FuncDef>,
    /// Lower-cased function name -> original spelling.
    func_names: HashMap<String, String>,
    /// Plugged-in provider of native functions (the full-runtime seam).
    host: Option<Box<dyn Host>>,
    /// OS integration; supplies builtins that cannot be portable.
    ///
    /// `None` until one is installed: the core deliberately names no concrete
    /// operating system (see `crate::platform`).
    platform: Option<Box<dyn Platform>>,
    /// Attached debugger (the debug-module seam).
    debugger: Option<Box<dyn Debugger>>,
    /// Breakpoints consulted before each statement.
    breakpoints: Breakpoints,
    error: i64,
    extended: i64,
    steps: u64,
    max_steps: u64,
    max_depth: usize,
    /// Set when a debugger asked to stop, read by [`Runtime::take_pause`].
    paused: Option<StopReason>,
    /// Pending unconditional jump (`jmp`): statements whose start line does
    /// not match are skipped without running until the target is reached.
    jump_target: Option<u32>,
    /// First..last line of the top-level statements, for `jump_to` validation.
    script_span: Option<(u32, u32)>,
    /// True while a debugger callback is running.
    ///
    /// The callback may evaluate expressions, which re-enters [`Runtime::exec_stmt`];
    /// without this guard a `print` inside a stop would recurse into the
    /// debugger for ever.
    in_debugger: bool,
    /// Set once a runtime error has been offered to the debugger.
    ///
    /// The hook fires at the innermost statement that failed; as the error
    /// propagates outward every enclosing `exec_stmt` sees it too, and this
    /// keeps it to one offer per error.
    error_reported: bool,
    /// Set by `Exit [code]`.
    exit_code: Option<i32>,
    /// Functions named by `OnAutoItExitRegister`, in registration order.
    ///
    /// AutoIt calls these when the process exits. A batch analysis run has no
    /// exit phase — the CLI stops after the script body and hands back the
    /// values it produced — so the names are recorded and left for the caller
    /// rather than invoked behind its back.
    exit_handlers: Vec<String>,
    /// Functions registered with `AdlibRegister`, with their intervals.
    ///
    /// AutoIt calls these whenever the script goes idle (inside `Sleep`, a
    /// message wait, ...). This interpreter has no idle clock and does not
    /// really wait, so they are recorded for the caller instead of being fired
    /// on a timer — the same reasoning as [`Runtime::exit_handlers`].
    adlib_handlers: Vec<AdlibHandler>,
    /// How faithfully AutoIt's observable behaviour is reproduced.
    profile: ExecutionProfile,
    /// Top-level (script) statements, executed by [`Runtime::run_script`].
    script: Vec<Stmt>,
}

/// How many `AdlibRegister` callbacks AutoIt accepts at once.
const ADLIB_LIMIT: usize = 10;

/// A function registered with `AdlibRegister`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdlibHandler {
    /// The name the script registered.
    pub name: String,
    /// How often AutoIt would call it, in milliseconds.
    pub interval_ms: i64,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugHost for Runtime {
    fn frames(&self) -> Vec<FrameInfo> {
        self.frames_snapshot()
    }

    fn globals(&self) -> Vec<(String, Value)> {
        self.globals_snapshot()
    }

    fn function_names(&self) -> Vec<String> {
        Runtime::function_names(self)
    }

    fn evaluate(&mut self, source: &str) -> Result<Value, RuntimeError> {
        // Evaluate in the frame the interpreter is stopped in, so `$x` means
        // what it means in the statement about to run.
        let span = self.frames.last().and_then(|f| f.span).unwrap_or_default();
        self.execute_source(source, span)
    }

    fn evaluate_expression(&mut self, source: &str) -> Result<Value, RuntimeError> {
        let span = self.frames.last().and_then(|f| f.span).unwrap_or_default();
        Runtime::evaluate_expression(self, source, span)
    }

    fn breakpoints(&self) -> Vec<Breakpoint> {
        self.breakpoints.items().to_vec()
    }

    fn add_breakpoint(&mut self, line: u32, condition: Option<String>) -> u32 {
        self.breakpoints.add(line, condition)
    }

    fn add_breakpoint_full(
        &mut self,
        line: u32,
        condition: Option<String>,
        skip: u64,
        every: u64,
        stop: bool,
        actions: Vec<String>,
    ) -> u32 {
        self.breakpoints
            .add_full(line, condition, skip, every, stop, actions)
    }

    fn remove_breakpoint(&mut self, id: u32) -> bool {
        self.breakpoints.remove(id)
    }

    fn set_breakpoint_enabled(&mut self, id: u32, enabled: bool) -> bool {
        self.breakpoints.set_enabled(id, enabled)
    }

    fn function_entry_line(&self, name: &str) -> Option<u32> {
        let def = self.funcs.get(&name.to_ascii_lowercase())?;
        Some(
            def.body
                .first()
                .map(|s| s.span.start.line)
                .unwrap_or_else(|| def.span.start.line),
        )
    }

    fn ignore_breakpoint(&mut self, id: u32, count: u64) -> bool {
        self.breakpoints.ignore(id, count)
    }

    fn set_breakpoint_stop(&mut self, id: u32, stop: bool) -> bool {
        self.breakpoints.set_stop(id, stop)
    }

    fn set_breakpoint_actions(&mut self, id: u32, actions: Vec<String>) -> bool {
        self.breakpoints.set_actions(id, actions)
    }

    fn jump_to(&mut self, line: u32) -> Result<(), RuntimeError> {
        // The target must live in the frame that will resume: the innermost
        // function call, or the script body at the top level.
        let in_range = match self.frames.last().and_then(|f| f.function.clone()) {
            Some(name) => self
                .funcs
                .get(&name.to_ascii_lowercase())
                .map(|d| line >= d.span.start.line && line <= d.span.end.line)
                .unwrap_or(false),
            None => self
                .script_span
                .map(|(lo, hi)| line >= lo && line <= hi)
                .unwrap_or(false),
        };
        if !in_range {
            return Err(RuntimeError::Unsupported {
                what: format!("jump target line {line} is outside the current frame"),
                span: None,
            });
        }
        self.jump_target = Some(line);
        Ok(())
    }
}

impl Runtime {
    /// Create a runtime with no program loaded.
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            frames: Vec::new(),
            funcs: HashMap::new(),
            func_names: HashMap::new(),
            host: None,
            platform: None,
            debugger: None,
            breakpoints: Breakpoints::new(),
            error: 0,
            extended: 0,
            steps: 0,
            max_steps: DEFAULT_MAX_STEPS,
            max_depth: DEFAULT_MAX_DEPTH,
            paused: None,
            jump_target: None,
            script_span: None,
            in_debugger: false,
            error_reported: false,
            exit_code: None,
            exit_handlers: Vec::new(),
            adlib_handlers: Vec::new(),
            profile: ExecutionProfile::default(),
            script: Vec::new(),
        }
    }

    /// Create a runtime with a program already loaded.
    pub fn with_program(prog: &Program) -> Self {
        let mut rt = Self::new();
        rt.load_program(prog);
        rt
    }

    /// The functions registered with `OnAutoItExitRegister`.
    pub fn exit_handlers(&self) -> &[String] {
        &self.exit_handlers
    }

    /// Record an `OnAutoItExitRegister` callback. See [`Runtime::exit_handlers`].
    pub(crate) fn register_exit_handler(&mut self, name: String) {
        if !self
            .exit_handlers
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&name))
        {
            self.exit_handlers.push(name);
        }
    }

    /// The functions registered with `AdlibRegister`.
    pub fn adlib_handlers(&self) -> &[AdlibHandler] {
        &self.adlib_handlers
    }

    /// Record an `AdlibRegister` callback, replacing an earlier registration of
    /// the same function. Returns false when the limit is reached.
    ///
    /// AutoIt allows at most ten at once.
    pub(crate) fn register_adlib(&mut self, name: String, interval_ms: i64) -> bool {
        if let Some(existing) = self
            .adlib_handlers
            .iter_mut()
            .find(|h| h.name.eq_ignore_ascii_case(&name))
        {
            existing.name = name;
            existing.interval_ms = interval_ms;
            return true;
        }
        if self.adlib_handlers.len() >= ADLIB_LIMIT {
            return false;
        }
        self.adlib_handlers.push(AdlibHandler { name, interval_ms });
        true
    }

    /// Forget an `AdlibRegister` callback. `None` forgets all of them.
    pub(crate) fn unregister_adlib(&mut self, name: Option<&str>) -> bool {
        let Some(name) = name else {
            let had = !self.adlib_handlers.is_empty();
            self.adlib_handlers.clear();
            return had;
        };
        match self
            .adlib_handlers
            .iter()
            .position(|h| h.name.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.adlib_handlers.remove(i);
                true
            }
            None => false,
        }
    }

    /// Forget an `OnAutoItExitRegister` callback.
    pub(crate) fn unregister_exit_handler(&mut self, name: &str) -> bool {
        match self
            .exit_handlers
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.exit_handlers.remove(i);
                true
            }
            None => false,
        }
    }

    /// Register every `Func` in `prog` so it can be called, and remember the
    /// top-level statements for [`Runtime::run_script`].
    ///
    /// Re-loading replaces previously loaded definitions.
    pub fn load_program(&mut self, prog: &Program) {
        self.script.clear();
        for item in &prog.items {
            self.load_item(item);
        }
    }

    /// Execute the program's top-level statements (script body), stopping at
    /// `Exit` or on error.
    ///
    /// `Func` definitions are *not* executed; call them explicitly with
    /// [`Runtime::call_function`].
    pub fn run_script(&mut self) -> Result<Flow, RuntimeError> {
        self.error_reported = false;
        let stmts = std::mem::take(&mut self.script);
        let mut flow = Flow::Normal;
        for s in &stmts {
            match self.exec_stmt(s) {
                Ok(Flow::Normal) => {}
                Ok(other) => {
                    flow = other;
                    break;
                }
                Err(e) => {
                    self.script = stmts;
                    self.notify_stop(StopReason::Error);
                    return Err(e);
                }
            }
        }
        self.script = stmts;
        self.jump_target = None;
        self.notify_stop(StopReason::Finished);
        Ok(flow)
    }

    fn load_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Func(f) => {
                let key = f.name.name.to_ascii_lowercase();
                self.func_names.insert(key.clone(), f.name.name.clone());
                self.funcs.insert(key, f.clone());
            }
            ItemKind::Stmt(st) => {
                let (lo, hi) = self.script_span.unwrap_or((u32::MAX, 0));
                self.script_span = Some((lo.min(st.span.start.line), hi.max(st.span.end.line)));
                self.script.push(st.clone());
            }
            ItemKind::Region(r) => {
                for it in &r.items {
                    self.load_item(it);
                }
            }
            _ => {}
        }
    }

    /// True when a user function with this name is loaded.
    pub fn has_function(&self, name: &str) -> bool {
        self.funcs.contains_key(&name.to_ascii_lowercase())
    }

    /// Names of all loaded user functions, in sorted order.
    pub fn function_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.func_names.values().cloned().collect();
        v.sort();
        v
    }

    /// Attach a host providing native functions.
    ///
    /// A host is consulted before the built-in [`Platform`], so an embedding
    /// application can override any platform-provided function.
    pub fn set_host(&mut self, host: Box<dyn Host>) {
        self.host = Some(host);
    }

    /// Install the platform layer.
    ///
    /// Nothing OS-specific can be reached until one is installed; use
    /// `autoitv3_platform::host_platform()` for the current OS.
    pub fn set_platform(&mut self, platform: Box<dyn Platform>) {
        self.platform = Some(platform);
    }

    /// The name of the installed platform, or `none` when there is not one.
    pub fn platform_name(&self) -> &'static str {
        self.platform.as_ref().map(|p| p.name()).unwrap_or("none")
    }

    /// Attach a debugger.
    pub fn set_debugger(&mut self, debugger: Box<dyn Debugger>) {
        self.debugger = Some(debugger);
    }

    /// Take the debugger back out (e.g. to read collected trace data).
    pub fn take_debugger(&mut self) -> Option<Box<dyn Debugger>> {
        self.debugger.take()
    }

    /// The breakpoint set the interpreter consults.
    pub fn breakpoints_mut(&mut self) -> &mut Breakpoints {
        &mut self.breakpoints
    }

    /// Take (and clear) a pending pause request.
    pub fn take_pause(&mut self) -> Option<StopReason> {
        self.paused.take()
    }

    /// Set the runaway-loop step budget (`0` disables the check).
    pub fn set_max_steps(&mut self, steps: u64) {
        self.max_steps = steps;
    }

    /// Set the recursion depth budget.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// The execution profile in force.
    pub fn profile(&self) -> &ExecutionProfile {
        &self.profile
    }

    /// Replace the execution profile.
    ///
    /// The default is [`ExecutionProfile::faithful`]; pass
    /// [`ExecutionProfile::deterministic`] when the goal is to *analyse* a
    /// script rather than run it (see [`crate::profile`]).
    pub fn set_profile(&mut self, profile: ExecutionProfile) {
        self.profile = profile;
    }

    /// The code passed to `Exit`, if the script exited.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// The current `@error` value.
    pub fn error(&self) -> i64 {
        self.error
    }

    /// The current `@extended` value.
    pub fn extended(&self) -> i64 {
        self.extended
    }

    /// Set `@error` / `@extended` (this is what `SetError()` does).
    pub fn set_error_value(&mut self, error: i64, extended: i64) {
        self.error = error;
        self.extended = extended;
    }

    /// Read a global variable by name (with or without the leading `$`).
    pub fn get_global(&self, name: &str) -> Option<&Value> {
        self.globals.get(&var_key(name))
    }

    /// Write a global variable.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(var_key(name), value);
    }

    /// Snapshot every global variable (for inspection / debugging).
    pub fn globals_snapshot(&self) -> Vec<(String, Value)> {
        let mut v: Vec<(String, Value)> =
            self.globals.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Snapshot the active call frames, innermost last (for debugging).
    pub fn frames_snapshot(&self) -> Vec<FrameInfo> {
        self.frames
            .iter()
            .enumerate()
            .map(|(i, f)| FrameInfo {
                function: f.function.clone(),
                depth: i,
                span: f.span,
                locals: {
                    let mut v: Vec<(String, Value)> =
                        f.vars.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    v
                },
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Calling into the program
    // ------------------------------------------------------------------

    /// Call a user function by name with the given arguments.
    ///
    /// This is the entry point the deobfuscator uses to evaluate the
    /// obfuscator's table-builder helpers.
    pub fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, RuntimeError> {
        self.error_reported = false;
        self.call_user(name, args, None)
    }

    /// Call a function value (as produced by evaluating a bare identifier),
    /// falling back to a builtin when no user function matches.
    pub fn call_value(&mut self, callee: &Value, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        let Value::FuncRef(name) = callee else {
            return Err(RuntimeError::Type {
                expected: "function reference",
                got: callee.type_name().to_string(),
                span: Some(span),
            });
        };
        let name = name.clone();
        self.call_named(&name, args, span)
    }

    /// Call `name` — user function first, then builtin, then host.
    pub fn call_named(&mut self, name: &str, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        if self.has_function(name) {
            return self.call_user(name, args, Some(span));
        }
        self.call_external(name, args, span)
    }

    /// Call a builtin or host function.
    ///
    /// The host is reached through a [`HostContext`] built from *disjoint*
    /// fields of `self`, so no raw pointers or interior mutability are needed.
    fn call_external(&mut self, name: &str, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        if let Some(v) = builtins::call(self, name, &args, span)? {
            return Ok(v);
        }
        // An explicit host wins over the platform default.
        if self.host.is_some() {
            let Runtime { globals, error, extended, profile, host, .. } = self;
            let mut ctx = HostBridge { globals, error, extended, profile };
            if let Some(host) = host.as_mut() {
                if let Some(v) = host.call(name, args.clone(), &mut ctx)? {
                    return Ok(v);
                }
            }
        }

        // Then the OS layer, when one is installed.
        let mut result = None;
        let mut pending: Vec<(String, Vec<Value>)> = Vec::new();
        if self.platform.is_some() {
            let Runtime { globals, error, extended, profile, platform, .. } = self;
            let mut ctx = HostBridge { globals, error, extended, profile };
            if let Some(p) = platform.as_mut() {
                result = p.call(name, args, &mut ctx)?;
                pending = p.take_pending_callbacks();
            }
        }

        // An emulated enumerator may have scheduled callback invocations
        // (`DllCallbackRegister` + `EnumWindows`): run the named AutoIt
        // functions now, after the platform call returned — the emulation
        // never re-enters the interpreter mid-call. `@error`/`@extended`
        // keep describing the platform call, not the callbacks.
        if !pending.is_empty() {
            let (err, ext) = (self.error, self.extended);
            for (cb_name, cb_args) in pending.into_iter().take(MAX_PENDING_CALLBACKS) {
                self.call_function(&cb_name, cb_args)?;
            }
            self.error = err;
            self.extended = ext;
        }

        if let Some(v) = result {
            return Ok(v);
        }

        Err(RuntimeError::UndefinedFunction { name: name.to_string(), span: Some(span) })
    }

    fn call_user(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Option<Span>,
    ) -> Result<Value, RuntimeError> {
        let key = name.to_ascii_lowercase();
        let Some(def) = self.funcs.get(&key).cloned() else {
            return Err(RuntimeError::UndefinedFunction {
                name: name.to_string(),
                span,
            });
        };

        if self.frames.len() >= self.max_depth {
            return Err(RuntimeError::CallDepthExceeded { limit: self.max_depth });
        }

        // Set up the frame with parameters bound.
        let mut vars: HashMap<String, Value> = HashMap::new();
        let mut by_ref: Vec<(String, Value)> = Vec::new();
        for (i, p) in def.params.iter().enumerate() {
            let arg = args.get(i).cloned().unwrap_or_else(|| match &p.default {
                Some(d) => self.eval_const_default(d),
                None => Value::Null,
            });
            let k = var_key(&p.name.name);
            vars.insert(k.clone(), arg.clone());
            if p.by_ref {
                by_ref.push((k, arg));
            }
        }

        let display = self
            .func_names
            .get(&key)
            .cloned()
            .unwrap_or_else(|| name.to_string());
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_call_enter(&display, &args);
        }

        self.frames.push(Frame {
            vars,
            function: Some(display.clone()),
            span,
            arg_count: args.len(),
        });

        let mut result = Ok(Value::Null);
        for st in &def.body {
            match self.exec_stmt(st) {
                Ok(Flow::Normal) => {}
                Ok(Flow::Return(v)) => {
                    result = Ok(v);
                    break;
                }
                Ok(Flow::Exit(code)) => {
                    // `Exit` terminates the whole script: record the code and
                    // unwind this call with a null result.
                    self.exit_code = Some(code);
                    result = Ok(Value::Null);
                    break;
                }
                // A loop- or case-control signal escaping a function is a
                // script bug; stop unwinding and report it.
                Ok(Flow::Break(_)) | Ok(Flow::Continue(_)) | Ok(Flow::ContinueCase) => {
                    result = Err(RuntimeError::Unsupported {
                        what: format!("loop or case control outside its block in {display}"),
                        span,
                    });
                    break;
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }

        let frame = self.frames.pop().expect("frame pushed above");
        // Copy-out `ByRef` parameters back into the caller's variable.
        for (k, _) in &by_ref {
            if let Some(v) = frame.vars.get(k) {
                let v = v.clone();
                self.write_back_by_ref(k, v);
            }
        }

        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_call_exit(&display, result.as_ref().ok());
        }
        self.jump_target = None;
        result
    }

    /// Copy a `ByRef` parameter back to the caller's variable of the same name.
    fn write_back_by_ref(&mut self, key: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            if frame.vars.contains_key(key) {
                frame.vars.insert(key.to_string(), value);
                return;
            }
        }
        // The caller passed a global.
        if self.globals.contains_key(key) {
            self.globals.insert(key.to_string(), value);
        }
    }

    /// Evaluate a parameter default (must not depend on frame state).
    fn eval_const_default(&mut self, e: &Expr) -> Value {
        self.eval_expr(e).unwrap_or(Value::Null)
    }

    // ------------------------------------------------------------------
    // Variables
    // ------------------------------------------------------------------

    fn current_vars(&mut self) -> &mut HashMap<String, Value> {
        if self.frames.is_empty() {
            &mut self.globals
        } else {
            &mut self.frames.last_mut().expect("non-empty").vars
        }
    }

    fn read_var(&self, name: &str, span: Span) -> Result<Value, RuntimeError> {
        let key = var_key(name);
        if let Some(f) = self.frames.last() {
            if let Some(v) = f.vars.get(&key) {
                return Ok(v.clone());
            }
        }
        if let Some(v) = self.globals.get(&key) {
            return Ok(v.clone());
        }
        // Unset variables read as "" in AutoIt when no `MustDeclareVars` is set.
        let _ = span;
        Ok(Value::Str(String::new()))
    }

    fn write_var(&mut self, name: &str, value: Value, scope: VarScope, span: Span) {
        let key = var_key(name);
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_variable_write(&key, &value);
        }
        let _ = span;
        match scope {
            VarScope::Global => {
                self.globals.insert(key, value);
            }
            VarScope::Local => {
                self.current_vars().insert(key, value);
            }
            VarScope::Auto => {
                // Prefer an existing binding: local first, then global.
                if let Some(f) = self.frames.last_mut() {
                    if f.vars.contains_key(&key) {
                        f.vars.insert(key, value);
                        return;
                    }
                }
                if self.globals.contains_key(&key) {
                    self.globals.insert(key, value);
                    return;
                }
                self.current_vars().insert(key, value);
            }
        }
    }

    /// Value of a variable for `Eval()`/`IsDeclared()`: the current frame
    /// first, then the globals. `None` means "not declared".
    pub fn variable_value(&self, name: &str) -> Option<Value> {
        let key = var_key(name);
        if let Some(f) = self.frames.last() {
            if let Some(v) = f.vars.get(&key) {
                return Some(v.clone());
            }
        }
        self.globals.get(&key).cloned()
    }

    /// Whether `name` names a variable that currently exists (`IsDeclared`).
    pub fn variable_declared(&self, name: &str) -> bool {
        self.variable_value(name).is_some()
    }

    /// `Assign()`: write `value` to a variable by name.
    ///
    /// `global` forces the global scope; `only_if_exists` mirrors
    /// `Assign($name, $value, 2)`, which refuses to create a new variable.
    /// Returns `false` for an invalid name or a refused create.
    pub fn assign_variable(
        &mut self,
        name: &str,
        value: Value,
        global: bool,
        only_if_exists: bool,
    ) -> bool {
        let key = var_key(name);
        if !is_valid_var_name(&key) {
            return false;
        }
        if only_if_exists && self.variable_value(&key).is_none() {
            return false;
        }
        let scope = if global { VarScope::Global } else { VarScope::Auto };
        self.write_var(&key, value, scope, Span::default());
        true
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    /// Evaluate an expression in the current frame.
    pub fn eval_expr(&mut self, e: &Expr) -> Result<Value, RuntimeError> {
        self.tick()?;
        match &e.kind {
            ExprKind::Lit(l) => Ok(match &l.kind {
                LitKind::Int(i) => Value::Int(*i),
                LitKind::Float(f) => Value::Float(*f),
                LitKind::Str(s) => Value::Str(s.clone()),
                LitKind::Bool(b) => Value::Bool(*b),
                LitKind::Default => Value::Default,
                LitKind::Null => Value::Null,
            }),
            ExprKind::Macro(name) => Ok(self.eval_macro(name)),
            ExprKind::Ident(id) => Ok(Value::FuncRef(id.name.clone())),
            ExprKind::Var(v) => {
                let base = self.read_var(&v.name.name, e.span)?;
                self.index_value(base, &v.indices, e.span)
            }
            ExprKind::Call(c) => {
                let mut args = Vec::with_capacity(c.args.len());
                for a in &c.args {
                    args.push(self.eval_expr(a)?);
                }
                self.call_named(&c.callee.name, args, e.span)
            }
            ExprKind::IndexCall(v, args) => {
                let callee = {
                    let base = self.read_var(&v.name.name, e.span)?;
                    self.index_value(base, &v.indices, e.span)?
                };
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval_expr(a)?);
                }
                self.call_value(&callee, argv, e.span)
            }
            ExprKind::Subscript(base, indices) => {
                let base = self.eval_expr(base)?;
                self.index_value(base, indices, e.span)
            }
            ExprKind::Unary(op, a) => {
                let v = self.eval_expr(a)?;
                Ok(match op {
                    UnaryOp::Not => Value::Bool(!v.is_truthy()),
                    UnaryOp::Neg => match v {
                        Value::Int(i) => Value::Int(-i),
                        other => Value::Float(-other.to_f64()),
                    },
                    UnaryOp::Plus => match v {
                        Value::Int(i) => Value::Int(i),
                        other => Value::Float(other.to_f64()),
                    },
                })
            }
            ExprKind::Binary(op, a, b) => self.eval_binary(op, a, b, e.span),
            ExprKind::Paren(p) => self.eval_expr(p),
            // Object member access resolves through the platform: property
            // reads for `Member`, method dispatch for `MethodCall`.
            ExprKind::Member(subject, name) => self.eval_member(subject, &name.name, e.span),
            ExprKind::MethodCall(subject, name, args) => {
                let mut evaluated = Vec::with_capacity(args.len());
                for a in args {
                    evaluated.push(self.eval_expr(a)?);
                }
                self.eval_method(subject, &name.name, evaluated, e.span)
            }
            ExprKind::WithSubject => {
                return Err(RuntimeError::Unsupported {
                    what: "`With` subject outside a platform host".to_string(),
                    span: Some(e.span),
                })
            }
            ExprKind::Ternary(c, x, y) => {
                if self.eval_expr(c)?.is_truthy() {
                    self.eval_expr(x)
                } else {
                    self.eval_expr(y)
                }
            }
            ExprKind::ArrayLit(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.eval_expr(it)?);
                }
                Ok(Value::array(out))
            }
        }
    }

    /// `$obj.Member` — property read through the platform.
    fn eval_member(
        &mut self,
        subject: &Expr,
        member: &str,
        span: autoitv3_ast::span::Span,
    ) -> Result<Value, RuntimeError> {
        let value = self.eval_expr(subject)?;
        let Value::Obj(obj) = value else {
            return Err(RuntimeError::Unsupported {
                what: format!("member access `.{member}` on a non-object value"),
                span: Some(span),
            });
        };
        let Runtime { globals, error, extended, profile, platform, .. } = self;
        let Some(platform) = platform.as_mut() else {
            return Err(RuntimeError::Unsupported {
                what: format!("member access `.{member}` (no platform installed)"),
                span: Some(span),
            });
        };
        let mut ctx = HostBridge { globals, error, extended, profile };
        platform
            .obj_get(&obj, member, &mut ctx)
            .map(|v| v.unwrap_or(Value::Null))
    }

    /// `$obj.Method(...)` — method dispatch through the platform.
    fn eval_method(
        &mut self,
        subject: &Expr,
        member: &str,
        args: Vec<Value>,
        span: autoitv3_ast::span::Span,
    ) -> Result<Value, RuntimeError> {
        let value = self.eval_expr(subject)?;
        let Value::Obj(obj) = value else {
            return Err(RuntimeError::Unsupported {
                what: format!("method call `.{member}()` on a non-object value"),
                span: Some(span),
            });
        };
        let Runtime { globals, error, extended, profile, platform, .. } = self;
        let Some(platform) = platform.as_mut() else {
            return Err(RuntimeError::Unsupported {
                what: format!("method call `.{member}()` (no platform installed)"),
                span: Some(span),
            });
        };
        let mut ctx = HostBridge { globals, error, extended, profile };
        platform
            .obj_call(&obj, member, &args, &mut ctx)
            .map(|v| v.unwrap_or(Value::Null))
    }

    fn eval_binary(
        &mut self,
        op: &BinaryOp,
        a: &Expr,
        b: &Expr,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        use BinaryOp::*;
        // Assignment operators write through to an lvalue.
        if matches!(op, Assign | PlusAssign | MinusAssign | StarAssign | SlashAssign | CaretAssign | AmpAssign) {
            let rhs = self.eval_expr(b)?;
            let value = match op {
                Assign => rhs,
                _ => {
                    let cur = self.eval_expr(a)?;
                    let arith = match op {
                        PlusAssign => Add,
                        MinusAssign => Sub,
                        StarAssign => Mul,
                        SlashAssign => Div,
                        CaretAssign => Pow,
                        AmpAssign => Concat,
                        _ => unreachable!(),
                    };
                    apply_arith(&cur, &rhs, &arith)
                }
            };
            self.assign_to(a, value.clone(), span)?;
            return Ok(value);
        }

        // Short-circuit `And` / `Or` like AutoIt does.
        match op {
            And => {
                let l = self.eval_expr(a)?;
                if !l.is_truthy() {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(self.eval_expr(b)?.is_truthy()));
            }
            Or => {
                let l = self.eval_expr(a)?;
                if l.is_truthy() {
                    return Ok(Value::Bool(true));
                }
                return Ok(Value::Bool(self.eval_expr(b)?.is_truthy()));
            }
            _ => {}
        }

        let l = self.eval_expr(a)?;
        let r = self.eval_expr(b)?;
        Ok(match op {
            // `==` is case-sensitive for strings; `=` and `<>` are not.
            Eq => Value::Bool(l.eq_strict(&r)),
            EqLoose => Value::Bool(l.eq_loose(&r)),
            NotEq => Value::Bool(!l.eq_loose(&r)),
            Lt => Value::Bool(l.compare(&r) == std::cmp::Ordering::Less),
            Le => Value::Bool(l.compare(&r) != std::cmp::Ordering::Greater),
            Gt => Value::Bool(l.compare(&r) == std::cmp::Ordering::Greater),
            Ge => Value::Bool(l.compare(&r) != std::cmp::Ordering::Less),
            Add | Sub | Mul | Div | Pow | Concat | BitAnd => apply_arith(&l, &r, op),
            Assign | PlusAssign | MinusAssign | StarAssign | SlashAssign | CaretAssign | AmpAssign => {
                unreachable!("handled above")
            }
            And | Or => unreachable!("handled above"),
        })
    }

    /// Write `value` into the lvalue expression `target`.
    fn assign_to(&mut self, target: &Expr, value: Value, span: Span) -> Result<(), RuntimeError> {
        match &target.kind {
            ExprKind::Var(v) if v.indices.is_empty() => {
                self.write_var(&v.name.name, value, VarScope::Auto, span);
                Ok(())
            }
            ExprKind::Var(v) => {
                let base = self.read_var(&v.name.name, span)?;
                self.assign_index(base, &v.indices, value, span)
            }
            other => Err(RuntimeError::Unsupported {
                what: format!("assignment target {other:?}"),
                span: Some(span),
            }),
        }
    }

    /// Apply array/map subscripts to `base`.
    fn index_value(
        &mut self,
        base: Value,
        indices: &[Expr],
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let mut cur = base;
        for idx in indices {
            let key = self.eval_expr(idx)?;
            cur = match &cur {
                Value::Array(a) => {
                    let i = key.to_int();
                    let arr = a.borrow();
                    if i < 0 || i as usize >= arr.len() {
                        return Err(RuntimeError::IndexOutOfBounds {
                            index: i,
                            len: arr.len(),
                            span: Some(span),
                        });
                    }
                    arr[i as usize].clone()
                }
                Value::Map(m) => {
                    let k = MapKey::from_value(&key);
                    m.borrow().get(&k).cloned().unwrap_or(Value::Null)
                }
                Value::Str(s) => {
                    // AutoIt strings are not indexable; return "".
                    let _ = s;
                    Value::Str(String::new())
                }
                Value::Null => Value::Null,
                other => {
                    return Err(RuntimeError::Type {
                        expected: "Array or Map",
                        got: other.type_name().to_string(),
                        span: Some(span),
                    })
                }
            };
        }
        Ok(cur)
    }

    /// Write into an array element or map key.
    fn assign_index(
        &mut self,
        base: Value,
        indices: &[Expr],
        value: Value,
        span: Span,
    ) -> Result<(), RuntimeError> {
        if indices.is_empty() {
            return Ok(());
        }
        // Evaluate the leading subscripts to find the container.
        let mut container = base;
        for idx in &indices[..indices.len() - 1] {
            let key = self.eval_expr(idx)?;
            container = match &container {
                Value::Array(a) => {
                    let i = key.to_int();
                    let arr = a.borrow();
                    if i < 0 || i as usize >= arr.len() {
                        return Err(RuntimeError::IndexOutOfBounds {
                            index: i,
                            len: arr.len(),
                            span: Some(span),
                        });
                    }
                    arr[i as usize].clone()
                }
                Value::Map(m) => {
                    let k = MapKey::from_value(&key);
                    m.borrow().get(&k).cloned().unwrap_or(Value::Null)
                }
                other => {
                    return Err(RuntimeError::Type {
                        expected: "Array or Map",
                        got: other.type_name().to_string(),
                        span: Some(span),
                    })
                }
            };
        }
        let last = &indices[indices.len() - 1];
        let key = self.eval_expr(last)?;
        match &container {
            Value::Array(a) => {
                let i = key.to_int();
                let mut arr = a.borrow_mut();
                if i < 0 || i as usize >= arr.len() {
                    return Err(RuntimeError::IndexOutOfBounds {
                        index: i,
                        len: arr.len(),
                        span: Some(span),
                    });
                }
                arr[i as usize] = value;
                Ok(())
            }
            Value::Map(m) => {
                m.borrow_mut().insert(MapKey::from_value(&key), value);
                Ok(())
            }
            other => Err(RuntimeError::Type {
                expected: "Array or Map",
                got: other.type_name().to_string(),
                span: Some(span),
            }),
        }
    }

    /// Resolve an AutoIt macro.
    ///
    /// The interpreter answers the macros that are pure interpreter state and
    /// defers everything environment-dependent to the installed platform, so
    /// `@TempDir` and friends are real values rather than empty strings.
    fn eval_macro(&self, name: &str) -> Value {
        let key = name.trim_start_matches('@').to_ascii_lowercase();
        match key.as_str() {
            // Interpreter state.
            "error" => Value::Int(self.error),
            "extended" => Value::Int(self.extended),
            "scriptlinenumber" => Value::Int(
                self.frames
                    .last()
                    .and_then(|f| f.span)
                    .map(|s| s.start.line as i64)
                    .unwrap_or(0),
            ),
            "numparams" => Value::Int(
                self.frames.last().map(|f| f.arg_count as i64).unwrap_or(0),
            ),
            // Universal character constants.
            "crlf" => Value::Str("\r\n".into()),
            "cr" => Value::Str("\r".into()),
            "lf" => Value::Str("\n".into()),
            "tab" => Value::Str("\t".into()),
            // Everything else is environment-dependent.
            _ => match self.platform.as_ref().and_then(|p| p.macro_value(&key)) {
                Some(v) => v,
                None => Value::Null,
            },
        }
    }

    fn tick(&mut self) -> Result<(), RuntimeError> {
        self.steps += 1;
        if self.max_steps != 0 && self.steps > self.max_steps {
            return Err(RuntimeError::StepLimitExceeded { limit: self.max_steps });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    /// Execute a statement, returning the control-flow signal it produced.
    ///
    /// A failure is offered to the debugger here, at the statement that raised
    /// it, before the error unwinds anything.
    pub fn exec_stmt(&mut self, s: &Stmt) -> Result<Flow, RuntimeError> {
        let result = self.exec_stmt_inner(s);
        if let Err(e) = &result {
            self.error_hook(e, Some(s.span));
        }
        result
    }

    fn exec_stmt_inner(&mut self, s: &Stmt) -> Result<Flow, RuntimeError> {
        self.tick()?;

        // An unconditional jump skips every statement until the target line
        // is reached; the matching statement runs and clears the target.
        if let Some(target) = self.jump_target {
            if s.span.start.line != target {
                return Ok(Flow::Normal);
            }
            self.jump_target = None;
        }
        // The frame's span is the statement being executed, so a debugger
        // inspecting a stopped frame sees the line it is stopped on.
        if let Some(f) = self.frames.last_mut() {
            f.span = Some(s.span);
        }
        self.debug_hook(s.span)?;
        // A `jmp` typed at the stop we just returned from targets *this*
        // statement onward: skip the current statement unless it is the target.
        if let Some(target) = self.jump_target {
            if s.span.start.line != target {
                return Ok(Flow::Normal);
            }
            self.jump_target = None;
        }

        match &s.kind {
            StmtKind::Directive(_) => Ok(Flow::Normal),
            StmtKind::Expr(e) => {
                self.eval_expr(e)?;
                Ok(Flow::Normal)
            }
            StmtKind::VarDecl(v) => {
                self.exec_var_decl(v, s.span)?;
                Ok(Flow::Normal)
            }
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Exit(e) => {
                let code = match e {
                    Some(e) => self.eval_expr(e)?.to_int() as i32,
                    None => 0,
                };
                Ok(Flow::Exit(code))
            }
            StmtKind::ExitLoop(e) => Ok(Flow::Break(loop_levels(self, e)?)),
            StmtKind::ContinueLoop(e) => Ok(Flow::Continue(loop_levels(self, e)?)),
            // The innermost Select/Switch consumes this; a loop passes it on
            // untouched, and a function boundary reports it as a mistake.
            StmtKind::ContinueCase => Ok(Flow::ContinueCase),
            StmtKind::If(if_) => self.exec_if(if_),
            StmtKind::While(w) => self.exec_while(w),
            StmtKind::DoUntil(d) => self.exec_do_until(d),
            StmtKind::For(f) => self.exec_for(f),
            // `Select` tests each case for truth; `Switch` compares against a
            // subject. Both share the fall-through, so both share the runner.
            StmtKind::Select(cases) => self.exec_case_list(cases, None),
            StmtKind::Switch(sw) => {
                let subject = self.eval_expr(&sw.expr)?;
                self.exec_case_list(&sw.cases, Some(&subject))
            }
            StmtKind::With(w) => {
                self.eval_expr(&w.expr)?;
                self.exec_block(&w.body)
            }
        }
    }

    fn exec_var_decl(&mut self, v: &VarDecl, span: Span) -> Result<(), RuntimeError> {
        let scope = match v.kind {
            VarKind::Global => VarScope::Global,
            VarKind::Local | VarKind::Dim | VarKind::Static => VarScope::Local,
        };
        // Running state for `Enum` numbering (ignored for other declarations).
        let enum_step = match &v.enum_step {
            Some(e) => self.eval_expr(e)?.to_int(),
            None => 1,
        };
        let mut enum_next: Option<i64> = None;

        for item in &v.vars {
            let key = var_key(&item.name.name);
            if v.is_redim {
                // `ReDim $a[n]` / `ReDim $a[n][m]` — resize in place, keeping
                // the values that still fit.
                let cur = self.read_var(&item.name.name, span)?;
                match (&cur, item.dims.len()) {
                    (Value::Array(a), dims) if dims > 1 => {
                        let rows = self.dim_size(&item.dims, span)?;
                        let cols = self.dim_size(&item.dims[1..], span)?;
                        let mut arr = a.borrow_mut();
                        while arr.len() < rows {
                            arr.push(Value::array_sized(cols));
                        }
                        arr.truncate(rows);
                        for row in arr.iter() {
                            if let Value::Array(r) = row {
                                r.borrow_mut().resize(cols, Value::Int(0));
                            }
                        }
                    }
                    (Value::Array(a), _) => {
                        let size = self.dim_size(&item.dims, span)?;
                        a.borrow_mut().resize(size, Value::Int(0));
                    }
                    _ => {
                        let value = self.array_with_dims(&item.dims, span)?;
                        self.write_var(&item.name.name, value, scope, span);
                    }
                }
                continue;
            }

            let value = if let Some(init) = &item.init {
                self.eval_expr(init)?
            } else if !item.dims.is_empty() {
                if is_empty_brackets(&item.dims) {
                    // `Local $m[]` declares a Map (an array needs a size or an
                    // initializer).
                    Value::map()
                } else {
                    self.array_with_dims(&item.dims, span)?
                }
            } else {
                Value::Str(String::new())
            };

            if v.is_enum {
                // Enum numbering: an explicit value resets the counter,
                // otherwise the member continues the sequence, advancing by
                // `Step n` (default 1).
                let value = match &item.init {
                    Some(init) => self.eval_expr(init)?.to_int(),
                    None => match enum_next {
                        Some(prev) => prev + enum_step,
                        None => 0,
                    },
                };
                enum_next = Some(value);
                self.write_var(&item.name.name, Value::Int(value), scope, span);
                continue;
            }
            let _ = key;
            self.write_var(&item.name.name, value, scope, span);
        }
        Ok(())
    }

    /// Build the array a declaration's dimensions describe.
    ///
    /// `$a[3]` is a flat array, `$a[3][4]` is three arrays of four — AutoIt
    /// nests the extra dimensions, and scripts index them with `$a[$i][$j]`.
    fn array_with_dims(&mut self, dims: &[Expr], span: Span) -> Result<Value, RuntimeError> {
        let Some(first) = dims.first() else {
            return Ok(Value::array_sized(0));
        };
        let n = if matches!(first.kind, ExprKind::Lit(Lit { kind: LitKind::Null, .. })) {
            0
        } else {
            let n = self.eval_expr(first)?.to_int();
            if n < 0 {
                return Err(RuntimeError::IndexOutOfBounds {
                    index: n,
                    len: 0,
                    span: Some(span),
                });
            }
            n as usize
        };
        if dims.len() == 1 {
            return Ok(Value::array_sized(n));
        }
        let mut rows = Vec::with_capacity(n);
        for _ in 0..n {
            rows.push(self.array_with_dims(&dims[1..], span)?);
        }
        Ok(Value::array(rows))
    }

    /// Size for `$a[n]`. Empty brackets (`$a[]`) are not a size — they are the
    /// "let the initializer decide" form, which yields an empty array.
    fn dim_size(&mut self, dims: &[Expr], span: Span) -> Result<usize, RuntimeError> {
        let Some(first) = dims.first() else { return Ok(0) };
        if matches!(first.kind, ExprKind::Lit(Lit { kind: LitKind::Null, .. })) {
            return Ok(0);
        }
        let n = self.eval_expr(first)?.to_int();
        if n < 0 {
            return Err(RuntimeError::IndexOutOfBounds {
                index: n,
                len: 0,
                span: Some(span),
            });
        }
        Ok(n as usize)
    }

    // ------------------------------------------------------------------
    // Debugger
    // ------------------------------------------------------------------

    /// Offer the statement about to run to the debugger, and stop if it asks.
    ///
    /// The debugger is moved out of its field for the duration of the callback
    /// so it can be handed `&mut self` — it needs the live interpreter to read
    /// frames and evaluate expressions. Because the field is empty while the
    /// callback runs, anything it evaluates (a `print`, a breakpoint condition)
    /// cannot re-enter the debugger.
    fn debug_hook(&mut self, span: Span) -> Result<(), RuntimeError> {
        if self.in_debugger {
            return Ok(());
        }
        let depth = self.frames.len();
        // A breakpoint only fires when its condition, if any, holds and its
        // hit rules allow it (skips first, then every-n). Ask before counting
        // the hit, so a guarded breakpoint's counter means what it says.
        let mut stopped = None;
        let mut action_bp: Option<Breakpoint> = None;
        if let Some(bp) = self.breakpoints.matching(span) {
            let (id, line, condition) = (bp.id, bp.line, bp.condition.clone());
            let fires = match &condition {
                None => true,
                Some(cond) => {
                    self.in_debugger = true;
                    let verdict = self
                        .evaluate_expression(cond, span)
                        .map(|v| v.is_truthy())
                        .unwrap_or(false);
                    self.in_debugger = false;
                    verdict
                }
            };
            if fires && self.breakpoints.should_fire(id) {
                let bp = self.breakpoints.get(id).cloned().unwrap_or_else(|| {
                    let mut b = Breakpoint::at_line(id, line);
                    b.hits = 1;
                    b
                });
                // On-hit actions are debugger commands (`print`, `eval`, ...),
                // run by the debugger with the live host.
                if !bp.actions.is_empty() {
                    action_bp = Some(bp.clone());
                }
                if bp.stop {
                    stopped = Some(StopReason::Breakpoint { id, line });
                }
            }
        }

        let Some(mut dbg) = self.debugger.take() else {
            if let Some(reason) = stopped {
                self.paused = Some(reason);
            }
            return Ok(());
        };
        self.in_debugger = true;
        if let Some(bp) = &action_bp {
            dbg.on_breakpoint_action(bp, self);
        }
        let action = dbg.on_statement(span, depth, self);

        if stopped.is_none() && matches!(action, DebugAction::Pause) {
            stopped = Some(StopReason::Step);
        }
        if let Some(reason) = &stopped {
            self.paused = Some(reason.clone());
            dbg.on_stop(reason, self);
        }
        self.in_debugger = false;
        self.debugger = Some(dbg);

        match action {
            DebugAction::Abort => Err(RuntimeError::Aborted),
            _ => Ok(()),
        }
    }

    /// Offer an uncaught error to the debugger before it unwinds.
    fn error_hook(&mut self, error: &RuntimeError, span: Option<Span>) {
        // A debugger-requested abort is a control signal, not a failure of the
        // program, and must not come back as a post-mortem stop.
        if self.in_debugger || self.error_reported || matches!(error, RuntimeError::Aborted) {
            return;
        }
        self.error_reported = true;
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        self.in_debugger = true;
        dbg.on_error(error, span, self);
        self.in_debugger = false;
        self.debugger = Some(dbg);
    }

    /// Run the debugger's end-of-run callback, if one is installed.
    fn notify_stop(&mut self, reason: StopReason) {
        if self.in_debugger {
            return;
        }
        let Some(mut dbg) = self.debugger.take() else {
            return;
        };
        self.in_debugger = true;
        dbg.on_stop(&reason, self);
        self.in_debugger = false;
        self.debugger = Some(dbg);
    }

    fn exec_if(&mut self, if_: &IfStmt) -> Result<Flow, RuntimeError> {
        if self.eval_expr(&if_.cond)?.is_truthy() {
            if let Some(ts) = &if_.then_stmt {
                return self.exec_stmt(ts);
            }
            return self.exec_block(&if_.then_block);
        }
        for (cond, body) in &if_.else_ifs {
            if self.eval_expr(cond)?.is_truthy() {
                return self.exec_block(body);
            }
        }
        self.exec_block(&if_.else_block)
    }

    fn exec_while(&mut self, w: &WhileStmt) -> Result<Flow, RuntimeError> {
        loop {
            if !self.eval_expr(&w.cond)?.is_truthy() {
                break;
            }
            match self.exec_block(&w.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => continue,
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_do_until(&mut self, d: &DoUntilStmt) -> Result<Flow, RuntimeError> {
        loop {
            match self.exec_block(&d.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => {}
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
            if self.eval_expr(&d.cond)?.is_truthy() {
                break;
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_for(&mut self, f: &ForStmt) -> Result<Flow, RuntimeError> {
        if let Some(iter) = &f.iter {
            // `For $x In $collection`
            let subject = self.eval_expr(iter)?;
            let items: Vec<Value> = match &subject {
                Value::Array(a) => a.borrow().clone(),
                Value::Map(m) => m.borrow().values().cloned().collect(),
                Value::Null => Vec::new(),
                other => vec![other.clone()],
            };
            for item in items {
                self.write_var(&f.var.name, item, VarScope::Auto, f.var.span);
                match self.exec_block(&f.body)? {
                    Flow::Normal => {}
                    Flow::Break(n) if n <= 1 => break,
                    Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                    Flow::Continue(n) if n <= 1 => continue,
                    Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                    other => return Ok(other),
                }
            }
            return Ok(Flow::Normal);
        }

        let from = self.eval_expr(&f.from)?;
        let to = self.eval_expr(&f.to)?;
        let step = match &f.step {
            Some(s) => self.eval_expr(s)?,
            None => Value::Int(1),
        };
        let stepf = step.to_f64();
        if stepf == 0.0 {
            return Ok(Flow::Normal);
        }
        let mut cur = from.to_f64();
        let end = to.to_f64();
        // Integer loop when everything is integral, so indices stay exact.
        let integral = matches!(from, Value::Int(_))
            && matches!(to, Value::Int(_))
            && matches!(step, Value::Int(_));
        loop {
            if stepf > 0.0 && cur > end {
                break;
            }
            if stepf < 0.0 && cur < end {
                break;
            }
            let v = if integral { Value::Int(cur as i64) } else { Value::Float(cur) };
            self.write_var(&f.var.name, v, VarScope::Auto, f.var.span);
            match self.exec_block(&f.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => {}
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
            cur += stepf;
        }
        Ok(Flow::Normal)
    }

    /// Run the cases of a `Switch` (with `subject`) or `Select` (`None`).
    ///
    /// A body that ends in `ContinueCase` re-enters at the **next** case
    /// without testing it — the fall-through AutoIt documents — so the search
    /// for the first match happens once and the running loop only advances.
    /// `ContinueCase` in the last case ends the block, as does running out.
    fn exec_case_list(
        &mut self,
        cases: &[CaseClause],
        subject: Option<&Value>,
    ) -> Result<Flow, RuntimeError> {
        let mut first = None;
        'cases: for (index, case) in cases.iter().enumerate() {
            if case.is_else {
                first = Some(index);
                break;
            }
            for value in &case.values {
                let tested = self.eval_expr(value)?;
                let matched = match subject {
                    Some(subject) => subject.eq_loose(&tested),
                    None => tested.is_truthy(),
                };
                if matched {
                    first = Some(index);
                    break 'cases;
                }
            }
        }
        let Some(mut index) = first else {
            return Ok(Flow::Normal);
        };
        loop {
            let flow = self.exec_block(&cases[index].body)?;
            if !matches!(flow, Flow::ContinueCase) {
                return Ok(flow);
            }
            index += 1;
            if index >= cases.len() {
                return Ok(Flow::Normal);
            }
        }
    }

    /// Execute a list of statements, propagating the first non-`Normal` flow.
    pub fn exec_block(&mut self, body: &[Stmt]) -> Result<Flow, RuntimeError> {
        for s in body {
            let flow = self.exec_stmt(s)?;
            if !flow.is_normal() {
                return Ok(flow);
            }
        }
        Ok(Flow::Normal)
    }

    /// Execute a string as AutoIt source (`Execute()`), in the current frame.
    /// Evaluate `source` as an **expression**, in the current frame.
    ///
    /// This is what a breakpoint condition and a debugger's `print` need, and
    /// it is not the same thing as [`Runtime::execute_source`]: `$i = 5` is an
    /// assignment when it stands alone as a statement, but a comparison inside
    /// an expression. A condition that silently assigned would both fire
    /// wrongly and corrupt the program it is watching, so conditions are always
    /// parsed in expression position — by way of a `Return`, which is the one
    /// place the grammar insists on one.
    pub fn evaluate_expression(&mut self, source: &str, span: Span) -> Result<Value, RuntimeError> {
        let wrapped = format!("Func __au3_expr__()\n    Return {source}\nEndFunc\n");
        let prog = autoitv3_ast::parse(&wrapped).map_err(|e| RuntimeError::Unsupported {
            what: format!("not an expression: {} ({source})", e.msg),
            span: Some(span),
        })?;
        let body = prog
            .items
            .iter()
            .find_map(|item| match &item.kind {
                ItemKind::Func(f) => Some(&f.body),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::Unsupported {
                what: format!("not an expression: {source}"),
                span: Some(span),
            })?;
        let expr = body.iter().find_map(|stmt| match &stmt.kind {
            StmtKind::Return(Some(e)) => Some(e),
            _ => None,
        });
        let Some(expr) = expr else {
            return Err(RuntimeError::Unsupported {
                what: format!("not an expression: {source}"),
                span: Some(span),
            });
        };
        // Evaluated in the caller's frame: `evaluate_expression` deliberately
        // does not push one, so `$local` means what it means at the stop.
        self.eval_expr(expr)
    }

    pub fn execute_source(&mut self, src: &str, span: Span) -> Result<Value, RuntimeError> {
        let prog = autoitv3_ast::parse(src).map_err(|e| RuntimeError::Unsupported {
            what: format!("Execute() parse error: {}", e.msg),
            span: Some(span),
        })?;
        // Only expression statements contribute a value, matching AutoIt's
        // `Execute` returning the value of the last expression.
        let mut last = Value::Null;
        for item in &prog.items {
            match &item.kind {
                ItemKind::Func(f) => {
                    let key = f.name.name.to_ascii_lowercase();
                    self.func_names.insert(key.clone(), f.name.name.clone());
                    self.funcs.insert(key, f.clone());
                }
                ItemKind::Stmt(s) => {
                    if let StmtKind::Expr(e) = &s.kind {
                        last = self.eval_expr(e)?;
                    } else {
                        match self.exec_stmt(s)? {
                            Flow::Normal => last = Value::Null,
                            Flow::Return(v) => return Ok(v),
                            other => {
                                return Err(RuntimeError::Unsupported {
                                    what: format!("Execute() control flow {other:?}"),
                                    span: Some(span),
                                })
                            }
                        }
                    }
                }
                ItemKind::Region(r) => {
                    for it in &r.items {
                        self.load_item(it);
                    }
                }
                ItemKind::Directive(_) => {}
            }
        }
        Ok(last)
    }
}

/// True when `e` is a *pure constant* expression: literals combined with
/// unary/binary/ternary operators, with no variables, macros, identifiers or
/// calls.
///
/// Deobfuscation uses this as the safety gate for inlining an evaluated
/// expression: a pure constant is guaranteed to have the same value wherever
/// it appears, which is exactly what constant folding needs.
pub fn is_constant_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Lit(_) => true,
        ExprKind::Paren(inner) => is_constant_expr(inner),
        ExprKind::Unary(_, a) => is_constant_expr(a),
        ExprKind::Binary(_, a, b) => is_constant_expr(a) && is_constant_expr(b),
        ExprKind::Ternary(c, a, b) => {
            is_constant_expr(c) && is_constant_expr(a) && is_constant_expr(b)
        }
        // Variables, macros, idents (function references) and calls all depend
        // on runtime state, so they are not constant.
        _ => false,
    }
}

/// True for the `$x[]` form — empty brackets, which the parser records as a
/// `Null` literal dimension.
fn is_empty_brackets(dims: &[Expr]) -> bool {
    dims.len() == 1
        && matches!(
            dims[0].kind,
            ExprKind::Lit(Lit { kind: LitKind::Null, .. })
        )
}

/// Variable names are case-insensitive in AutoIt and stored without the `$`.
fn var_key(name: &str) -> String {
    name.trim_start_matches('$').to_ascii_lowercase()
}

/// A variable name is a non-empty identifier: ASCII letters, digits and `_`,
/// not starting with a digit.
fn is_valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Where a variable write should land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarScope {
    /// Force the global scope (`Global`).
    Global,
    /// Force the current frame (`Local` / `Dim` / `Static`).
    Local,
    /// Existing binding wins, else the current frame.
    Auto,
}

fn loop_levels(rt: &mut Runtime, e: &Option<Expr>) -> Result<usize, RuntimeError> {
    match e {
        Some(e) => {
            let n = rt.eval_expr(e)?.to_int();
            Ok(if n < 1 { 1 } else { n as usize })
        }
        None => Ok(1),
    }
}

/// Numeric / string arithmetic shared by binary operators and compound assigns.
fn apply_arith(l: &Value, r: &Value, op: &BinaryOp) -> Value {
    use BinaryOp::*;
    match op {
        Concat => Value::Str(format!("{}{}", l.to_autoit_string(), r.to_autoit_string())),
        BitAnd => {
            // `&` between non-strings is bitwise AND in AutoIt.
            if matches!(l, Value::Str(_)) || matches!(r, Value::Str(_)) {
                Value::Str(format!("{}{}", l.to_autoit_string(), r.to_autoit_string()))
            } else {
                Value::Int(l.to_int() & r.to_int())
            }
        }
        _ => {
            let both_int = matches!(l, Value::Int(_)) && matches!(r, Value::Int(_));
            let (a, b) = (l.to_f64(), r.to_f64());
            match op {
                Add => int_or_float(both_int, a + b),
                Sub => int_or_float(both_int, a - b),
                Mul => int_or_float(both_int, a * b),
                Div => {
                    if b == 0.0 {
                        // AutoIt divides by zero -> 0 with @error set; we keep
                        // it simple and return 0.
                        Value::Int(0)
                    } else if both_int && (l.to_int() % r.to_int()) == 0 {
                        Value::Int(l.to_int() / r.to_int())
                    } else {
                        Value::Float(a / b)
                    }
                }
                Pow => int_or_float(both_int, a.powf(b)),
                _ => Value::Null,
            }
        }
    }
}

fn int_or_float(both_int: bool, v: f64) -> Value {
    if both_int && v.fract() == 0.0 && v.abs() < 9.0e15 {
        Value::Int(v as i64)
    } else {
        Value::Float(v)
    }
}

/// Adapts the interpreter's variable/error state to the [`HostContext`] seam
/// without borrowing the whole [`Runtime`].
struct HostBridge<'a> {
    globals: &'a mut HashMap<String, Value>,
    error: &'a mut i64,
    extended: &'a mut i64,
    profile: &'a ExecutionProfile,
}

impl HostContext for HostBridge<'_> {
    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.get(&var_key(name)).cloned()
    }

    fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(var_key(name), value);
    }

    fn error(&self) -> i64 {
        *self.error
    }

    fn set_error(&mut self, error: i64, extended: i64) {
        *self.error = error;
        *self.extended = extended;
    }

    fn profile(&self) -> &ExecutionProfile {
        self.profile
    }
}