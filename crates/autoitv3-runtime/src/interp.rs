//! The interpreter core.
//!
//! [`Runtime`] loads a parsed [`Program`](autoitv3_ast::ast::Program) and can
//! call into it. It is deliberately small but semantically careful about the
//! parts of AutoIt the obfuscator leans on:
//!
//! * `ByRef` parameters — copy-in/copy-out, not a true alias. A bare `$name`
//!   argument goes back to the variable the call site named; an element
//!   (`$a[$i]`, `$m["k"]`) goes back through the container the call site
//!   captured, whose subscripts are evaluated exactly once. A literal or an
//!   expression has no target and is left alone (AutoIt rejects literals)
//! * `Static` locals, which outlive the call and are shared by every activation
//! * `ReDim` resizing an array in place
//! * `For ... To ... Step` and `For ... In`
//! * compound assignment (`+=`, `&=`, ...)
//! * `@error` / `@extended`
//!
//! It also exposes the seams a debugger and a full runtime need — see
//! [`crate::debug`] and [`crate::host`].

use std::collections::HashMap;
use std::rc::Rc;

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;
use autoitv3_i18n::msg;

use crate::builtins;
use crate::debug::{
    Breakpoint, Breakpoints, DebugAction, DebugHost, Debugger, FrameInfo, StopReason,
};
use crate::error::{Flow, RuntimeError};
use crate::host::{Host, HostContext};
use crate::platform::Platform;
use crate::profile::ExecutionProfile;
use crate::value::{FuncRefName, MapKey, Value};

/// Default runaway-loop guard.
///
/// The single budget every entry point starts from: a bare [`Runtime`] and the
/// deobfuscation passes (which evaluate the obfuscator's own table builders) all
/// use this value, so there is one default to reason about. `0` disables the
/// check; callers override it with [`Runtime::set_max_steps`].
pub const DEFAULT_MAX_STEPS: u64 = 20_000_000;
/// The most elements one array may hold — AutoIt's own ceiling.
///
/// From the help's *Appendix → Limits / Defaults* table: `VAR_SUBSCRIPT_ELEMENTS`
/// is "16,777,216: Maximum number of elements for an array", counted across all
/// the dimensions. A script that asks for more is stopped here; the official
/// interpreter refuses it too, and an obfuscated `$n + 4294967295` (four billion
/// elements) would otherwise take the process down inside the allocator.
const MAX_ARRAY_ELEMENTS: i64 = 16_777_216;

/// Default recursion guard.
pub const DEFAULT_MAX_DEPTH: usize = 256;
/// The instant the deterministic profile reports: 2024-01-01T00:00:00Z.
///
/// The clock macros have to answer *something*, and a fixed instant keeps a
/// deobfuscation run reproducible (`SRandom(@MSEC & @SEC & @MIN)` would
/// otherwise seed differently every time).
const DETERMINISTIC_CLOCK_MS: i64 = 1_704_067_200_000;

/// Upper bound on callback invocations drained after a single platform call,
/// so a scripted enumerator cannot flood the run.
const MAX_PENDING_CALLBACKS: usize = 10_000;

/// One activation record.
struct Frame {
    vars: HashMap<String, Value>,
    /// Names this frame resolves to a function-level `Static`: variable key ->
    /// key into [`Runtime::statics`].
    ///
    /// AutoIt's `Static` is one variable shared by every call of the function,
    /// so a frame only remembers where the storage lives — it does not hold a
    /// copy, or a recursive call's write-back would resurrect a stale value.
    statics: HashMap<String, String>,
    function: Option<String>,
    span: Option<Span>,
    /// How many arguments the caller passed, for `@NumParams`.
    arg_count: usize,
    /// The parameter names, in declaration order, so the debugger can print a
    /// frame the way gdb does (`func(a=1, b="x")`). A `HashMap` has no order.
    params: Vec<String>,
    /// Whether this frame called `SetError` (`extended_set`: `SetExtended`).
    ///
    /// Only what the function itself sets survives its return: a nested call
    /// leaves its `@error` visible *inside* the function, but a function that
    /// never called `SetError` reports 0 to its caller. See
    /// [`Runtime::call_user_def`].
    error_set: bool,
    extended_set: bool,
}

/// What the build under analysis tells the script about itself.
///
/// The three macros a script uses to ask "who am I running as" — `@Compiled`,
/// `@Unicode`, `@AutoItX64` — are not properties of the machine emulating it
/// now; they are properties of the interpreter the script was built for. A
/// `.exe` analysed through its extracted source has to keep answering them the
/// way it did when it ran, or every branch on them goes the wrong way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildFacts {
    /// `@Compiled`: the script came out of a `.exe`/`.a3x`.
    pub compiled: bool,
    /// `@Unicode`: the interpreter is a Unicode build. AutoIt dropped ANSI in
    /// 3.3.14, so this is `true` for everything modern.
    pub unicode: bool,
    /// `@AutoItX64`, when the build says; `None` leaves it to the platform,
    /// which answers from the emulated machine (`--win-arch`).
    pub autoit_x64: Option<bool>,
}

impl Default for BuildFacts {
    fn default() -> Self {
        Self {
            compiled: false,
            unicode: true,
            autoit_x64: None,
        }
    }
}

/// The AutoIt interpreter.
pub struct Runtime {
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    /// Function-level `Static` variables, keyed by function and name.
    ///
    /// They outlive a call, which is the whole point of `Static`; a script uses
    /// them for session state (an open handle, a cache) that the next call must
    /// still see.
    statics: HashMap<String, Value>,
    /// Lower-cased function name -> definition.
    funcs: HashMap<String, Rc<FuncDef>>,
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
    /// The statement currently executing, for error reporting.
    current_span: Option<Span>,
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
    /// True while a debugger *expression* is being evaluated — the `print`,
    /// `watch` and breakpoint-condition path, which is always
    /// [`Runtime::evaluate_expression`].
    ///
    /// An expression typed at the prompt is a question about the program, so
    /// reading a variable that was never assigned is answered with
    /// `undefined variable: $x` instead of the empty string a script gets. The
    /// typo is what the user needs to see; `""` looks like a real value. Script
    /// code is unaffected: it keeps AutoIt's own behaviour (see
    /// [`Runtime::read_var_key`]).
    strict_reads: bool,
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
    /// Whether the script is running as a compiled build (`@Compiled`).
    ///
    /// A build's own script answered `1` while it was running; a plain `.au3`
    /// run under `AutoIt3.exe` answers `0`. Matching that keeps a script that
    /// branches on `@Compiled` on the path it actually took.
    compiled: bool,
    /// Whether the interpreter is a Unicode build (`@Unicode`).
    ///
    /// AutoIt dropped the ANSI build in 3.3.14 and added the macro to report
    /// which one is running, so a modern build answers `1`; only an embedder
    /// replaying something older has a reason to turn this off.
    unicode: bool,
    /// Which `@AutoItX64` answers, when the build under analysis says something
    /// other than the platform's own machine. `None` asks the platform.
    autoit_x64: Option<bool>,
    /// Top-level (script) statements, executed by [`Runtime::run_script`].
    script: Vec<Stmt>,
    /// The function `#OnAutoItStartRegister` names, if the program carries the
    /// directive: AutoIt's compiler calls it once, before the body's first
    /// statement, at every script start.
    start_register: Option<String>,
    /// The `Opt`/`AutoItSetOption` settings a script changed, by lower-cased
    /// option name.
    ///
    /// Options are read by two very different things: the runtime answers the
    /// return value of `Opt` (the *previous* setting) and a platform reads the
    /// ones that describe its own behaviour, through
    /// [`HostContext::option`](crate::host::HostContext::option).
    options: HashMap<String, Value>,
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

    fn evaluate_in_frame(&mut self, depth: usize, source: &str) -> Result<Value, RuntimeError> {
        let hidden = self.hide_frames_below(depth);
        let span = self.frames.last().and_then(|f| f.span).unwrap_or_default();
        let out = self.execute_source(source, span);
        self.frames.extend(hidden);
        out
    }

    fn evaluate_expression_in_frame(
        &mut self,
        depth: usize,
        source: &str,
    ) -> Result<Value, RuntimeError> {
        let hidden = self.hide_frames_below(depth);
        let span = self.frames.last().and_then(|f| f.span).unwrap_or_default();
        let out = Runtime::evaluate_expression(self, source, span);
        self.frames.extend(hidden);
        out
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
                what: msg!("jump target line {line} is outside the current frame", line = line),
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
            statics: HashMap::new(),
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
            current_span: None,
            max_depth: DEFAULT_MAX_DEPTH,
            paused: None,
            jump_target: None,
            script_span: None,
            in_debugger: false,
            strict_reads: false,
            error_reported: false,
            exit_code: None,
            exit_handlers: Vec::new(),
            adlib_handlers: Vec::new(),
            profile: ExecutionProfile::default(),
            compiled: false,
            unicode: true,
            autoit_x64: None,
            script: Vec::new(),
            start_register: None,
            options: HashMap::new(),
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
        self.start_register = start_register(prog);
    }

    /// Execute the program's top-level statements (script body), stopping at
    /// `Exit` or on error.
    ///
    /// `Func` definitions are *not* executed; call them explicitly with
    /// [`Runtime::call_function`].
    pub fn run_script(&mut self) -> Result<Flow, RuntimeError> {
        self.error_reported = false;
        // `#OnAutoItStartRegister "F"`: AutoIt's compiler registers a call to
        // `F` that runs once at each start, before the body's first statement.
        // A name that is not defined fails the way any other missing function
        // does.
        if let Some(name) = self.start_register.clone() {
            self.call_function(&name, Vec::new())?;
        }
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
                self.funcs.insert(key, Rc::new(f.clone()));
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

    /// Set `@Compiled`: true when the script came from a compiled build.
    ///
    /// The CLI derives this from the input (a `.exe`/`.a3x` answers 1, a
    /// `.au3` answers 0), so a script that relaunches itself or strips its own
    /// command line takes the branch its build would have taken.
    pub fn set_compiled(&mut self, compiled: bool) {
        self.compiled = compiled;
    }

    /// Install the build facts the script observes through its macros.
    ///
    /// This is the seam for "the script did not run under *this* interpreter,
    /// it ran under the build these facts describe": a `.exe` analysed through
    /// its extracted source answered `@AutoItX64` from its own PE header, not
    /// from whatever machine is emulating it now.
    pub fn set_build_facts(&mut self, facts: BuildFacts) {
        self.compiled = facts.compiled;
        self.unicode = facts.unicode;
        self.autoit_x64 = facts.autoit_x64;
    }

    /// Set `@Unicode`: whether the interpreter is a Unicode build.
    pub fn set_unicode(&mut self, unicode: bool) {
        self.unicode = unicode;
    }

    /// Set `@AutoItX64` to the build's own answer; `None` leaves it to the
    /// platform (the emulated machine).
    pub fn set_autoit_x64(&mut self, x64: Option<bool>) {
        self.autoit_x64 = x64;
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

    /// Set `@error` / `@extended`.
    ///
    /// This is what *every* builtin does — a failing `FileOpen` and a
    /// `SetError` call both land here — so it does not by itself make the codes
    /// survive the current function's return; [`Runtime::mark_error_set`] is
    /// what `SetError`/`SetExtended` add on top.
    pub fn set_error_value(&mut self, error: i64, extended: i64) {
        self.error = error;
        self.extended = extended;
    }

    /// Record that the running function called `SetError` (`extended`: that it
    /// called `SetExtended`), which is what lets the codes outlive it.
    pub(crate) fn mark_error_set(&mut self, extended: bool) {
        if let Some(frame) = self.frames.last_mut() {
            if extended {
                frame.extended_set = true;
            } else {
                frame.error_set = true;
            }
        }
    }

    /// An `Opt` setting, as the script last set it.
    pub fn option(&self, name: &str) -> Option<&Value> {
        self.options.get(&var_key(name))
    }

    /// Record an `Opt` setting.
    pub(crate) fn set_option(&mut self, name: &str, value: Value) {
        self.options.insert(var_key(name), value);
    }

    /// Read a global variable by name (with or without the leading `$`).
    pub fn get_global(&self, name: &str) -> Option<&Value> {
        self.globals.get(&var_key(name))
    }

    /// Write a global variable.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(var_key(name), value);
    }

    /// Define the predefined `$CmdLine` / `$CmdLineRaw` a script sees.
    ///
    /// AutoIt exposes a program's command line as a 1-based array whose element
    /// 0 is the count (`$CmdLine[0]`), plus the raw text (`$CmdLineRaw`).
    /// Setting both keeps a script's own argument handling working when the
    /// interpreter runs it.
    pub fn set_cmdline(&mut self, args: &[String]) {
        let mut line = Vec::with_capacity(args.len() + 1);
        line.push(Value::Int(args.len() as i64));
        line.extend(args.iter().cloned().map(Value::Str));
        self.set_global("CmdLine", Value::array(line));

        let raw = args
            .iter()
            .map(|arg| quote_cmdline_arg(arg))
            .collect::<Vec<_>>()
            .join(" ");
        self.set_global("CmdLineRaw", Value::Str(raw));
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
                params: f
                    .params
                    .iter()
                    .filter_map(|k| f.vars.get(k).map(|v| (k.clone(), v.clone())))
                    .collect(),
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
        // The reference is shared and carries its own lookup key, so neither
        // the name nor the key has to be copied here.
        self.call_named_key(name.key(), name.display(), args, span)
    }

    /// Call `name` — user function first, then builtin, then host.
    pub fn call_named(&mut self, name: &str, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        let key = var_key(name);
        self.call_named_key(&key, name, args, span)
    }

    /// [`Runtime::call_named`] for callers that already hold the lookup key
    /// (`FuncRefName::key`, `Ident::key`).
    ///
    /// `display` is the name as the script wrote it, used for error messages and
    /// the debugger hook; `key` drives the lookups, so no call has to lower-case
    /// a name or allocate one.
    fn call_named_key(
        &mut self,
        key: &str,
        display: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let Some(def) = self.funcs.get(key).cloned() else {
            return self.call_external(key, display, args, span);
        };
        // Values only: these entry points have no call site to read, so a
        // `ByRef` parameter falls back to writing back by parameter name.
        self.call_user_def(def, args, None, Some(span))
    }

    /// Evaluate a call's arguments and invoke the user function they name,
    /// gathering the `ByRef` write-back targets on the way.
    ///
    /// This is the call-site path (`Foo($a[0])`): it has the argument
    /// *expressions*, and the callee's definition, so it can both decide which
    /// arguments need a target and evaluate a subscript exactly once for the
    /// value *and* for the path back.
    fn call_user_args(
        &mut self,
        def: Rc<FuncDef>,
        args: &[Expr],
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let gather = def.params.iter().any(|p| p.by_ref);
        let mut values = Vec::with_capacity(args.len());
        // One slot per argument; only built when the callee has `ByRef`
        // parameters at all, so an ordinary call pays nothing for this.
        let mut targets = if gather { Some(Vec::with_capacity(args.len())) } else { None };
        for (i, a) in args.iter().enumerate() {
            let by_ref = gather && def.params.get(i).is_some_and(|p| p.by_ref);
            if by_ref {
                let (value, target) = self.eval_call_arg(a, span)?;
                values.push(value);
                targets.as_mut().expect("gather implies Some").push(Some(target));
            } else {
                values.push(self.eval_expr(a)?);
                if let Some(t) = targets.as_mut() {
                    t.push(None);
                }
            }
        }
        self.call_user_def(def, values, targets, Some(span))
    }

    /// Evaluate one argument of a `ByRef` parameter, together with where its
    /// final value goes back to.
    fn eval_call_arg<'a>(
        &mut self,
        a: &'a Expr,
        span: Span,
    ) -> Result<(Value, ByRefTarget<'a>), RuntimeError> {
        let ExprKind::Var(v) = &a.kind else {
            // A literal or an expression has no variable to write back to.
            return Ok((self.eval_expr(a)?, ByRefTarget::Nothing));
        };
        if v.indices.is_empty() {
            return Ok((self.eval_expr(a)?, ByRefTarget::Caller(v.name.key())));
        }
        // `$a[...]`: evaluate the subscripts *once* and use them both for the
        // value and for the write-back path — re-evaluating them later could
        // run their side effects twice, or see a changed index.
        let base = self.read_var_key(&v.name)?;
        let mut keys = Vec::with_capacity(v.indices.len());
        for idx in &v.indices {
            keys.push(self.eval_expr(idx)?);
        }
        let value = index_by_keys(&base, &keys, span)?;
        let target = match &base {
            // The container is shared (`Rc`), so writing through it is visible
            // to the caller without any copy-out.
            Value::Array(_) | Value::Map(_) => ByRefTarget::Elem { base, keys },
            // `$s[0]` on a string, or on a null: no element to write into.
            _ => ByRefTarget::Nothing,
        };
        Ok((value, target))
    }

    /// The `DllCall` slots the caller declared as pointers: whether the return
    /// value is one, and which argument positions are.
    ///
    /// Both are *positions*, so they can be read off whichever array the OS layer
    /// hands back - and they are worked out before the call, because the call
    /// takes `args` by value.
    fn dll_pointer_slots(key: &str, args: &[Value]) -> (bool, Vec<usize>) {
        if key != "dllcall" {
            return (false, Vec::new());
        }
        let declared =
            |raw: &str| raw.split(':').next().unwrap_or(raw).trim().to_ascii_lowercase();
        let retval = args.get(1).is_some_and(|t| declared(&t.to_autoit_string()) == "ptr");
        let mut writes = Vec::new();
        for (index, pair) in args.get(3..).unwrap_or(&[]).chunks(2).enumerate() {
            if pair.len() == 2 && declared(&pair[0].to_autoit_string()) == "ptr*" {
                // The result array is `[return value, arg1, arg2, ...]`.
                writes.push(index + 1);
            }
        }
        (retval, writes)
    }

    /// The slots above, marked as pointers on the way out.
    ///
    /// AutoIt's `Ptr` is a base type, so the pointer-ness has to travel *with the
    /// value*: a `ptr` return and a `ptr*` write-back become `Value::Ptr`, while a
    /// plain `ptr` argument's slot simply echoes what the caller passed (with the
    /// type the caller passed it as). Functions whose result is a pointer whatever
    /// it is (`Ptr`, `DllStructGetPtr`, `DllCallbackGetPtr`) are the other source.
    fn tag_pointers(key: &str, slots: (bool, Vec<usize>), mut result: Value) -> Value {
        match key {
            "ptr" | "dllstructgetptr" | "dllcallbackgetptr" => {
                if let Value::Int(i) = result {
                    result = Value::Ptr(i);
                }
            }
            "dllcall" => {
                if let Value::Array(items) = &result {
                    let (retval, writes) = slots;
                    let mut items = items.borrow_mut();
                    fn mark(v: &mut Value) {
                        if let Value::Int(i) = v {
                            *v = Value::Ptr(*i);
                        }
                    }
                    if retval {
                        if let Some(v) = items.first_mut() {
                            mark(v);
                        }
                    }
                    for index in writes {
                        if let Some(v) = items.get_mut(index) {
                            mark(v);
                        }
                    }
                }
            }
            _ => {}
        }
        result
    }

    /// Call a builtin or host function.

    ///
    /// The host is reached through a [`HostContext`] built from *disjoint*
    /// fields of `self`, so no raw pointers or interior mutability are needed.
    fn call_external(
        &mut self,
        key: &str,
        display: &str,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        // Builtins/host/platform calls have no script body, so the debugger
        // gets its own hook for them (`untilcall GUICreate` is one user). The
        // hook wants the spelling the script used.
        // AutoIt resets both error codes before every builtin (`FunctionExecute`
        // in the interpreter's source, which is why `@error` after a successful
        // builtin is always 0). A host or a platform function is one of those
        // builtins, so a stale code cannot leak through one either.
        self.error = 0;
        self.extended = 0;
        // The OS layer takes `args` by value, so the `DllCall` types that name the
        // pointer slots are read off here, before the call happens.
        let pointer_slots = Self::dll_pointer_slots(key, &args);
        // `span` is the call and `self.frames.len()` the frame making it, so a
        // catchpoint can show the call site even when the arguments were
        // computed by calls of their own (the last statement to run would then
        // be inside the callee).
        let frame_depth = self.frames.len();
        let call_action = match self.debugger.as_mut() {
            Some(dbg) => dbg.on_builtin_call(display, &args, span, frame_depth),
            None => DebugAction::Continue,
        };
        match call_action {
            // A `stopat`-style stop: suspend *before* the call, so the debugger
            // can show the arguments and the call site. `notify_stop` takes the
            // debugger out and back, and ignores a stop asked for from inside
            // the prompt itself.
            DebugAction::Pause => {
                let reason = StopReason::Call { name: display.to_string() };
                self.paused = Some(reason.clone());
                self.notify_stop(reason);
            }
            DebugAction::Abort => return Err(RuntimeError::Aborted),
            DebugAction::Continue => {}
        }
        if let Some(v) = builtins::call(self, key, &args, span)? {
            return Ok(Self::tag_pointers(key, pointer_slots, v));
        }
        // An explicit host wins over the platform default.
        if self.host.is_some() {
            let Runtime { globals, error, extended, profile, options, host, .. } = self;
            let mut ctx = HostBridge { globals, error, extended, profile, options };
            if let Some(host) = host.as_mut() {
                if let Some(v) = host.call(display, args.clone(), &mut ctx)? {
                    return Ok(Self::tag_pointers(key, pointer_slots, v));
                }
            }
        }

        // Then the OS layer, when one is installed.
        let mut result = None;
        let mut pending: Vec<(String, Vec<Value>)> = Vec::new();
        if self.platform.is_some() {
            let Runtime { globals, error, extended, profile, options, platform, .. } = self;
            let mut ctx = HostBridge { globals, error, extended, profile, options };
            if let Some(p) = platform.as_mut() {
                result = p.call(display, args, &mut ctx)?;
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
            return Ok(Self::tag_pointers(key, pointer_slots, v));
        }

        Err(RuntimeError::UndefinedFunction { name: display.to_string(), span: Some(span) })
    }

    fn call_user(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Option<Span>,
    ) -> Result<Value, RuntimeError> {
        let key = var_key(name);
        self.call_user_key(&key, name, args, span)
    }

    /// [`Runtime::call_user`] for callers that already hold the lookup key.
    fn call_user_key(
        &mut self,
        key: &str,
        display: &str,
        args: Vec<Value>,
        span: Option<Span>,
    ) -> Result<Value, RuntimeError> {
        let Some(def) = self.funcs.get(key).cloned() else {
            return Err(RuntimeError::UndefinedFunction {
                name: display.to_string(),
                span,
            });
        };
        self.call_user_def(def, args, None, span)
    }

    /// Invoke a user function whose definition is already in hand.
    ///
    /// `targets` is the call site's `ByRef` write-back plan, one slot per
    /// argument: `Some(..)` means the call site was read (a slot that is `None`
    /// had nothing to write back to); `None` means there was no call site —
    /// a callback or the public `call_*` API — where the parameter name is the
    /// only thing left to copy out to.
    fn call_user_def(
        &mut self,
        def: Rc<FuncDef>,
        args: Vec<Value>,
        targets: Option<Vec<Option<ByRefTarget<'_>>>>,
        span: Option<Span>,
    ) -> Result<Value, RuntimeError> {
        if self.frames.len() >= self.max_depth {
            return Err(RuntimeError::CallDepthExceeded { limit: self.max_depth });
        }

        // Set up the frame with parameters bound. `ByRef` parameters remember
        // where their final value goes (see [`ByRefTarget`]).
        let mut targets = targets;
        let mut vars: HashMap<String, Value> = HashMap::new();
        let mut by_ref: Vec<(String, ByRefTarget<'_>, Value)> = Vec::new();
        for (i, p) in def.params.iter().enumerate() {
            let arg = args.get(i).cloned().unwrap_or_else(|| match &p.default {
                Some(d) => self.eval_const_default(d),
                None => Value::Null,
            });
            let k = var_key(&p.name.name);
            vars.insert(k.clone(), arg.clone());
            if p.by_ref {
                let target = match targets.as_mut() {
                    Some(t) => t
                        .get_mut(i)
                        .and_then(Option::take)
                        .unwrap_or(ByRefTarget::Nothing),
                    None => ByRefTarget::ParamName,
                };
                by_ref.push((k, target, arg));
            }
        }

        let display = def.name.name.clone();

        self.frames.push(Frame {
            vars,
            statics: HashMap::new(),
            function: Some(display.clone()),
            span,
            arg_count: args.len(),
            params: def
                .params
                .iter()
                .map(|p| var_key(&p.name.name))
                .collect(),
            error_set: false,
            extended_set: false,
        });

        // The hook runs with the frame live, so a `stopat` stop here can read
        // the parameters, and `on_call_enter`'s action decides whether the body
        // runs at all.
        let call_action = match self.debugger.as_mut() {
            Some(dbg) => dbg.on_call_enter(&display, &args),
            None => DebugAction::Continue,
        };
        match call_action {
            DebugAction::Pause => {
                let reason = StopReason::Call { name: display.clone() };
                self.paused = Some(reason.clone());
                self.notify_stop(reason);
            }
            DebugAction::Abort => return Err(RuntimeError::Aborted),
            DebugAction::Continue => {}
        }

        // "When entering a user-written function @error macro is set to 0"
        // (SetError's help page; `Parser_UserFunctionCall` does the same for
        // `@extended`).
        self.error = 0;
        self.extended = 0;

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
                        what: msg!("loop or case control outside its block in {display}", display = display),
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
        // Leaving a function: the codes survive only when this frame set them
        // itself. `SetError` makes a function's error its own; a nested call's
        // error is *visible* while the body runs but is gone once it returns
        // ("The value of @error is not maintained after popping the stack a
        // second time", and `SetError(1)` followed by `Sleep(1000)` reports 0 —
        // the call after `SetError` is what replaces the value).
        if !frame.error_set {
            self.error = 0;
        }
        if !frame.extended_set {
            self.extended = 0;
        }
        // Copy-out `ByRef` parameters back into the caller's variable.
        for (k, target, _) in &by_ref {
            let Some(value) = frame.vars.get(k).cloned() else { continue };
            match target {
                ByRefTarget::Caller(name) => self.write_back_by_ref(name, value),
                ByRefTarget::ParamName => self.write_back_by_ref(k, value),
                ByRefTarget::Elem { base, keys } => {
                    // Straight into the shared container, so no copy-out step
                    // is needed. Best effort: if the array shrank or changed
                    // shape while the call ran, there is nowhere to put it.
                    let _ = store_index_path(base, keys, value, span.unwrap_or_default());
                }
                ByRefTarget::Nothing => {}
            }
        }

        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_call_exit(&display, result.as_ref().ok());
        }
        self.jump_target = None;
        result
    }

    /// Copy a `ByRef` parameter's final value back to the caller's variable
    /// `key` (a lower-cased lookup key, the form [`Ident::key`] produces).
    ///
    /// The binding follows the same precedence an ordinary write would: a
    /// `Static`, then a local in the caller's frame, then a global. A name the
    /// caller has bound to nothing yet is created in its own frame — that is
    /// what `Bump($x)` does to an `$x` the caller had only read from.
    fn write_back_by_ref(&mut self, key: &str, value: Value) {
        // The caller may have passed one of its `Static` variables; that write
        // has to reach the function's storage, not a per-call slot.
        if let Some(store) = self.frames.last().and_then(|f| f.statics.get(key)).cloned() {
            bind_var(&mut self.statics, &store, value);
            return;
        }
        if let Some(frame) = self.frames.last_mut() {
            if frame.vars.contains_key(key) {
                bind_var(&mut frame.vars, key, value);
                return;
            }
        }
        // The caller passed a global.
        if self.globals.contains_key(key) {
            bind_var(&mut self.globals, key, value);
            return;
        }
        bind_var(self.current_vars(), key, value);
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

    /// Read the variable `name` refers to.
    ///
    /// The lookup key is cached on the identifier, so a read costs neither an
    /// allocation nor a lower-casing — this is the interpreter's hottest loop.
    ///
    /// A name that resolves nowhere reads as `""` under AutoIt's default
    /// options, which is what a script expects. While a debugger expression is
    /// being evaluated ([`Runtime::strict_reads`]) it is an
    /// [`RuntimeError::UndefinedVariable`] instead: someone typed that name at
    /// a prompt, and a typo reported as `""` looks like a value the program
    /// really holds. The error carries no span, because the expression was
    /// parsed from a generated wrapper and its positions mean nothing to the
    /// user; the name is the whole message.
    fn read_var_key(&self, name: &Ident) -> Result<Value, RuntimeError> {
        let key = name.key();
        if let Some(f) = self.frames.last() {
            if let Some(store) = f.statics.get(key) {
                return Ok(self.statics.get(store).cloned().unwrap_or(Value::Null));
            }
            if let Some(v) = f.vars.get(key) {
                return Ok(v.clone());
            }
        }
        if let Some(v) = self.globals.get(key) {
            return Ok(v.clone());
        }
        if self.strict_reads {
            return Err(RuntimeError::UndefinedVariable {
                name: name.name.clone(),
                span: None,
            });
        }
        // Unset variables read as "" in AutoIt when no `MustDeclareVars` is set.
        Ok(Value::Str(String::new()))
    }

    fn write_var(&mut self, name: &str, value: Value, scope: VarScope, span: Span) {
        self.write_var_key(&var_key(name), value, scope, span)
    }

    /// [`Runtime::write_var`] for callers that already hold the lookup key.
    ///
    /// Rebinding an existing variable goes through `get_mut`, which reuses the
    /// key the map already owns instead of allocating a copy of it.
    fn write_var_key(&mut self, key: &str, value: Value, scope: VarScope, span: Span) {
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_variable_write(key, &value);
        }
        let _ = span;
        // A name bound to a `Static` writes through to the function's storage,
        // so every activation sees the same variable.
        if !matches!(scope, VarScope::Global) {
            if let Some(store) = self.frames.last().and_then(|f| f.statics.get(key)).cloned() {
                bind_var(&mut self.statics, &store, value);
                return;
            }
        }
        match scope {
            VarScope::Global => bind_var(&mut self.globals, key, value),
            VarScope::Local => bind_var(self.current_vars(), key, value),
            VarScope::Auto => {
                // Prefer an existing binding: local first, then global.
                if let Some(f) = self.frames.last_mut() {
                    if let Some(slot) = f.vars.get_mut(key) {
                        *slot = value;
                        return;
                    }
                }
                if let Some(slot) = self.globals.get_mut(key) {
                    *slot = value;
                    return;
                }
                bind_var(self.current_vars(), key, value);
            }
        }
    }

    /// Value of a variable for `Eval()`/`IsDeclared()`: the current frame
    /// first, then the globals. `None` means "not declared".
    pub fn variable_value(&self, name: &str) -> Option<Value> {
        let key = var_key(name);
        if let Some(f) = self.frames.last() {
            if let Some(store) = f.statics.get(&key) {
                return self.statics.get(store).cloned();
            }
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
            ExprKind::Ident(id) => Ok(Value::FuncRef(FuncRefName::new(&id.name))),
            ExprKind::Var(v) => {
                let base = self.read_var_key(&v.name)?;
                self.index_value(base, &v.indices, e.span)
            }
            ExprKind::Call(c) => {
                // A user function is called from its definition, so the
                // arguments can be evaluated with the `ByRef` write-back plan
                // in hand (see `call_user_args`).
                if let Some(def) = self.funcs.get(c.callee.key()).cloned() {
                    return self.call_user_args(def, &c.args, e.span);
                }
                let mut args = Vec::with_capacity(c.args.len());
                for a in &c.args {
                    args.push(self.eval_expr(a)?);
                }
                self.call_external(c.callee.key(), &c.callee.name, args, e.span)
            }
            ExprKind::IndexCall(v, args) => {
                let callee = {
                    let base = self.read_var_key(&v.name)?;
                    self.index_value(base, &v.indices, e.span)?
                };
                // `$table[i](...)`: the callee resolved to a function value, so
                // a user function is reachable by key exactly as above.
                if let Value::FuncRef(name) = &callee {
                    if let Some(def) = self.funcs.get(name.key()).cloned() {
                        return self.call_user_args(def, args, e.span);
                    }
                }
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
            ExprKind::Binary(op, a, b) => {
                // `And`/`Or` are **short-circuit**, and scripts rely on it: AutoIt's
                // own failure idiom is `If (@error Or Not $arr[0])`, where `$arr` is
                // a *scalar* when the call failed - evaluating the right side would
                // be the fatal "expected Array or Map" error instead of the handler.
                // Measured on the official 3.3.16 x64 interpreter (under wine):
                // `If (1 = 1 Or $x[0])` leaves `$x[0]` unevaluated (its fatal
                // subscript error never fires) and `If (1 = 0 And $x[0])` likewise,
                // while `If (1 = 0 Or $x[0])` really does evaluate it and dies.
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    let truth = self.eval_expr(a)?.is_truthy();
                    let short = match op {
                        BinaryOp::And => !truth,
                        _ => truth,
                    };
                    if short {
                        return Ok(Value::Bool(truth));
                    }
                    return Ok(Value::Bool(self.eval_expr(b)?.is_truthy()));
                }
                self.eval_binary(op, a, b, e.span)
            }
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
                    what: msg!("`With` subject outside a platform host"),
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
                what: msg!("member access `.{member}` on a non-object value", member = member),
                span: Some(span),
            });
        };
        let Runtime { globals, error, extended, profile, options, platform, .. } = self;
        let Some(platform) = platform.as_mut() else {
            return Err(RuntimeError::Unsupported {
                what: msg!("member access `.{member}` (no platform installed)", member = member),
                span: Some(span),
            });
        };
        let mut ctx = HostBridge { globals, error, extended, profile, options };
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
                what: msg!("method call `.{member}()` on a non-object value", member = member),
                span: Some(span),
            });
        };
        let Runtime { globals, error, extended, profile, options, platform, .. } = self;
        let Some(platform) = platform.as_mut() else {
            return Err(RuntimeError::Unsupported {
                what: msg!("method call `.{member}()` (no platform installed)", member = member),
                span: Some(span),
            });
        };
        let mut ctx = HostBridge { globals, error, extended, profile, options };
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

    /// Run an expression statement — one whose value is thrown away.
    ///
    /// A bare `$s &= x` (or the `$s = $s & x` spelling of it) is the
    /// obfuscator's string builder, and the statement does not need the
    /// concatenated result. Going through `eval_expr` makes `apply_arith`
    /// allocate a fresh `String` and copy the whole accumulator on every
    /// iteration, which is quadratic in the final length; appending to the
    /// buffer the variable already owns keeps such a loop linear.
    fn exec_expr_stmt(&mut self, e: &Expr, span: Span) -> Result<(), RuntimeError> {
        if let ExprKind::Binary(op, target, value) = &e.kind {
            if let ExprKind::Var(t) = &target.kind {
                if t.indices.is_empty() {
                    match op {
                        // `$s &= x` — the right side is evaluated first, just
                        // as the generic operator path does, so a side effect it
                        // has on the target is already in the slot below.
                        BinaryOp::AmpAssign => {
                            let rhs = self.eval_expr(value)?;
                            self.append_value_str(t.name.key(), &rhs, span);
                            return Ok(());
                        }
                        // `$s = $s & x` — the same accumulation written the
                        // long way. Appending here reads the target *after* the
                        // right side has run, so it stays equivalent only while
                        // nothing in that right side can reassign it; a
                        // side-effect-free operand guarantees exactly that.
                        BinaryOp::Assign => {
                            if let ExprKind::Binary(BinaryOp::Concat, left, right) = &value.kind {
                                if let ExprKind::Var(l) = &left.kind {
                                    if l.indices.is_empty()
                                        && t.name.name.eq_ignore_ascii_case(&l.name.name)
                                        && expr_is_pure(right)
                                    {
                                        let rhs = self.eval_expr(right)?;
                                        self.append_value_str(t.name.key(), &rhs, span);
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let _ = self.eval_expr(e)?;
        Ok(())
    }

    /// Append the string form of `value` to the variable `key` names.
    fn append_value_str(&mut self, key: &str, value: &Value, span: Span) {
        match value {
            Value::Str(s) => self.append_var_str(key, s, span),
            other => self.append_var_str(key, &other.to_autoit_string(), span),
        }
    }

    /// Append `text` to the variable `key` names, declaring it as `text` when
    /// it is not bound yet.
    ///
    /// AutoIt strings are values and every read clones, so a variable slot owns
    /// its buffer outright: mutating it here cannot be observed through a value
    /// read earlier.
    fn append_var_str(&mut self, key: &str, text: &str, span: Span) {
        // The same lookup order `read_var` uses: the innermost frame shadows
        // the globals.
        if let Some(frame) = self.frames.last_mut() {
            if let Some(slot) = frame.vars.get_mut(key) {
                append_text(slot, text);
                if let Some(dbg) = self.debugger.as_mut() {
                    dbg.on_variable_write(key, slot);
                }
                return;
            }
        }
        if let Some(slot) = self.globals.get_mut(key) {
            append_text(slot, text);
            if let Some(dbg) = self.debugger.as_mut() {
                dbg.on_variable_write(key, slot);
            }
            return;
        }
        // An unset variable reads as "", so `$s &= x` declares it holding `x`.
        self.write_var_key(key, Value::Str(text.to_string()), VarScope::Auto, span);
    }

    /// Write `value` into the lvalue expression `target`.
    fn assign_to(&mut self, target: &Expr, value: Value, span: Span) -> Result<(), RuntimeError> {
        match &target.kind {
            ExprKind::Var(v) if v.indices.is_empty() => {
                self.write_var_key(v.name.key(), value, VarScope::Auto, span);
                Ok(())
            }
            ExprKind::Var(v) => {
                let base = self.read_var_key(&v.name)?;
                self.assign_index(base, &v.indices, value, span)
            }
            other => Err(RuntimeError::Unsupported {
                what: msg!("assignment target {target}", target = format!("{other:?}")),
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
            cur = index_step(&cur, &key, span)?;
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
            container = index_step(&container, &key, span)?;
        }
        let last = &indices[indices.len() - 1];
        let key = self.eval_expr(last)?;
        store_index(&container, &key, value, span)
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
            // Script state: which interpreter the script is (or was) running
            // under. See [`BuildFacts`].
            "compiled" => Value::Int(self.compiled as i64),
            "unicode" => Value::Int(self.unicode as i64),
            "autoitx64" => match self.autoit_x64 {
                Some(x64) => Value::Int(x64 as i64),
                // No build to ask: the emulated machine answers.
                None => self.platform_macro(&key),
            },
            // The clock. AutoIt reports local time; this reports the fixed
            // instant the deterministic profile runs at, or the host's time
            // otherwise (see [`Runtime::clock_parts`]).
            "year" | "mon" | "mday" | "hour" | "min" | "sec" | "msec" | "wday" | "yday" => {
                self.clock_macro(&key)
            }
            // Universal character constants.
            "crlf" => Value::Str("\r\n".into()),
            "cr" => Value::Str("\r".into()),
            "lf" => Value::Str("\n".into()),
            "tab" => Value::Str("\t".into()),
            // Everything else is environment-dependent.
            _ => self.platform_macro(&key),
        }
    }

    /// The installed platform's answer for one macro.
    ///
    /// OS identity, the emulated machine and the directory macros live there;
    /// a macro nobody answers is empty rather than an error, which is what
    /// AutoIt does for a macro it does not know.
    fn platform_macro(&self, key: &str) -> Value {
        match self.platform.as_ref().and_then(|p| p.macro_value(key)) {
            Some(v) => v,
            None => Value::Null,
        }
    }

    /// One of the clock macros, from [`Runtime::clock_parts`].
    ///
    /// AutoIt returns these as zero-padded strings (`@MON` is `01`..`12`,
    /// `@MSEC` is `000`..`999`, `@YDAY` is `001`..`366`), which is what makes
    /// `@MSEC & @SEC & @MIN` a fixed-width seed.
    fn clock_macro(&self, key: &str) -> Value {
        let (year, month, day, hour, minute, second, weekday, yearday) = self.clock_parts();
        let text = match key {
            "year" => format!("{year:04}"),
            "mon" => format!("{month:02}"),
            "mday" => format!("{day:02}"),
            "hour" => format!("{hour:02}"),
            "min" => format!("{minute:02}"),
            "sec" => format!("{second:02}"),
            "msec" => format!("{:03}", self.clock_millis() % 1000),
            "wday" => weekday.to_string(),
            "yday" => format!("{yearday:03}"),
            _ => return Value::Null,
        };
        Value::Str(text)
    }

    /// Milliseconds since the Unix epoch: fixed for a deterministic run, the
    /// host clock otherwise.
    fn clock_millis(&self) -> i64 {
        if self.profile.is_deterministic() {
            DETERMINISTIC_CLOCK_MS
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(DETERMINISTIC_CLOCK_MS)
        }
    }

    /// `(@YEAR, @MON, @MDAY, @HOUR, @MIN, @SEC, @WDAY, @YDAY)`.
    ///
    /// AutoIt reports local time; this decomposes the instant in UTC, which is
    /// where it has to stop without a timezone database. Scripts use the clock
    /// overwhelmingly to seed `Random`, so the offset does not change what they
    /// compute. `@WDAY` is 1 (Sunday) through 7 (Saturday), `@YDAY` is 1-based.
    fn clock_parts(&self) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
        let millis = self.clock_millis();
        let days = millis.div_euclid(86_400_000);
        let ms_of_day = millis.rem_euclid(86_400_000);
        let hour = ms_of_day / 3_600_000;
        let minute = (ms_of_day / 60_000) % 60;
        let second = (ms_of_day / 1_000) % 60;
        let (year, month, day) = civil_from_days(days);
        // 1970-01-01 was a Thursday; AutoIt counts Sunday as 1.
        let weekday = (days.rem_euclid(7) + 4) % 7 + 1;
        let yearday = days - days_from_civil(year, 1, 1) + 1;
        (year, month, day, hour, minute, second, weekday, yearday)
    }

    fn tick(&mut self) -> Result<(), RuntimeError> {
        self.steps += 1;
        if self.max_steps != 0 && self.steps > self.max_steps {
            return Err(RuntimeError::StepLimitExceeded {
                limit: self.max_steps,
                span: self.current_span,
            });
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
        // Remember where we are before spending the step: a runaway loop is
        // reported at the statement it is spinning on.
        self.current_span = Some(s.span);
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
                self.exec_expr_stmt(e, s.span)?;
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
            let key = item.name.key();
            if v.is_redim {
                // `ReDim $a[n]` / `ReDim $a[n][m]` — resize in place, keeping
                // the values that still fit.
                let cur = self.read_var_key(&item.name)?;
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
                        self.write_var_key(item.name.key(), value, scope, span);
                    }
                }
                continue;
            }

            // A function-level `Static` is one variable the function shares
            // with every call: the first call evaluates the initializer and
            // parks the value on the runtime, later calls leave it alone and
            // only point this frame at the storage that already holds it.
            // (Outside a function there is nothing to share with, so `Static`
            // falls through to the ordinary path below.)
            if v.kind == VarKind::Static && !self.frames.is_empty() {
                let store = self.static_store_key(key);
                if !self.statics.contains_key(&store) {
                    let value = self.decl_value(item, span)?;
                    self.statics.insert(store.clone(), value);
                }
                self.frames
                    .last_mut()
                    .expect("non-empty")
                    .statics
                    .insert(key.to_string(), store);
                continue;
            }

            let value = self.decl_value(item, span)?;

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
                self.write_var_key(item.name.key(), Value::Int(value), scope, span);
                continue;
            }
            let _ = key;
            self.write_var_key(item.name.key(), value, scope, span);
        }
        Ok(())
    }

    /// Take off the frames deeper than `depth` (the selected one), so a
    /// variable lookup starts there and walks outward. The caller puts them
    /// back with `self.frames.extend(..)`.
    fn hide_frames_below(&mut self, depth: usize) -> Vec<Frame> {
        if depth + 1 < self.frames.len() {
            self.frames.split_off(depth + 1)
        } else {
            Vec::new()
        }
    }

    /// The value a declaration gives one variable: its initializer, else the
    /// array its brackets describe, else an empty string.
    fn decl_value(&mut self, item: &VarDeclItem, span: Span) -> Result<Value, RuntimeError> {
        if let Some(init) = &item.init {
            let literals = self.eval_expr(init)?;
            if item.dims.is_empty() {
                return Ok(literals);
            }
            // Every bracket that names a size keeps it, and every empty bracket
            // (`[]`) takes its extent from the initializer. Measured on the
            // official x64 interpreter: `Local $a[][2] = [[1,2],[3,4],[5,6]]` is
            // 3x2, `Local $a[2][] = [[1,2],[3,4]]` is 2x2, `Local $a[][][] =
            // [[[1,2],[3,4]],[[5,6],[7,8]]]` is 2x2x2, a literal shorter than a
            // declared dimension is padded with empty strings
            // (`Local $a[][3] = [[1,2,3],[4]]` is 2x3, `Local $b[4] = [9]` has
            // `$b[0] = 9` and `StringLen($b[1]) = 0`), and `Local $a[] = 5` is
            // an empty array.
            let extents = literal_extents(&literals, item.dims.len());
            let mut shape = Vec::with_capacity(item.dims.len());
            let mut auto = false;
            for (depth, dim) in item.dims.iter().enumerate() {
                if dim_is_empty(dim) {
                    auto = true;
                    shape.push(extents.get(depth).copied().unwrap_or(0));
                    continue;
                }
                let n = self.eval_expr(dim)?.to_int();
                if n < 0 {
                    return Err(RuntimeError::IndexOutOfBounds {
                        index: n,
                        len: 0,
                        span: Some(span),
                    });
                }
                shape.push(n as usize);
            }
            if !auto && !matches!(literals, Value::Array(_)) {
                // A scalar initializer for a fixed shape stops the script -
                // measured: `Local $a[3] = 5` never reaches the next statement.
                return Err(RuntimeError::Type {
                    expected: "Array",
                    got: literals.type_name().to_string(),
                    span: Some(span),
                });
            }
            let target = self.array_from_shape(&shape, span)?;
            fill_from_literal(&target, &literals, span)?;
            return Ok(target);
        }
        if !item.dims.is_empty() {
            if is_empty_brackets(&item.dims) {
                // `Local $m[]` declares a Map (an array needs a size or an
                // initializer).
                return Ok(Value::map());
            }
            return self.array_with_dims(&item.dims, span);
        }
        Ok(Value::Str(String::new()))
    }

    /// Storage key for a function-level `Static` variable.
    ///
    /// The store lives on the runtime and therefore has to be qualified by the
    /// declaring function: two functions may each declare `Static $cache` and
    /// those are two different variables. The separator cannot appear in a
    /// variable name, so no name pair can collide.
    fn static_store_key(&self, var_key: &str) -> String {
        let function = self
            .frames
            .last()
            .and_then(|f| f.function.as_deref())
            .unwrap_or("");
        format!("{}\u{1}{}", function.to_ascii_lowercase(), var_key)
    }

    /// Build the array a declaration's dimensions describe.
    ///
    /// `$a[3]` is a flat array, `$a[3][4]` is three arrays of four — AutoIt
    /// nests the extra dimensions, and scripts index them with `$a[$i][$j]`.
    fn array_with_dims(&mut self, dims: &[Expr], span: Span) -> Result<Value, RuntimeError> {
        self.array_with_dims_total(dims, span, 1)
    }

    /// Build an array of empty strings from already-resolved dimensions.
    ///
    /// `[3]` is a flat array, `[3, 4]` is three arrays of four — the same
    /// nesting [`array_with_dims`](Self::array_with_dims) builds, without
    /// evaluating anything. A cell nothing has written is an empty string, not
    /// 0: measured, `VarGetType($e[0])` of `Local $e[3]` is "String" with
    /// `StringLen` 0.
    fn array_from_shape(&self, shape: &[usize], span: Span) -> Result<Value, RuntimeError> {
        let elements = shape.iter().try_fold(1i64, |acc, n| acc.checked_mul(*n as i64));
        match elements {
            Some(n) if n <= MAX_ARRAY_ELEMENTS => {}
            _ => {
                return Err(RuntimeError::ArrayTooLarge {
                    elements: elements.unwrap_or(i64::MAX),
                    limit: MAX_ARRAY_ELEMENTS,
                    span: Some(span),
                })
            }
        }
        fn build(shape: &[usize]) -> Value {
            match shape.split_first() {
                None => Value::Str(String::new()),
                Some((n, rest)) if rest.is_empty() => {
                    Value::array(vec![Value::Str(String::new()); *n])
                }
                Some((n, rest)) => Value::array((0..*n).map(|_| build(rest)).collect()),
            }
        }
        Ok(build(shape))
    }

    /// [`array_with_dims`](Self::array_with_dims) with the element count the
    /// outer dimensions have already asked for.
    ///
    /// AutoIt's own limit is `VAR_SUBSCRIPT_ELEMENTS` — "16,777,216: maximum
    /// number of elements for an array" (the help's *Appendix → Limits /
    /// Defaults* table) — counted across every dimension. A declaration past it
    /// is refused here instead of being handed to the allocator: an obfuscated
    /// expression such as `$n + 4294967295` asks for four billion elements, and
    /// the process would die inside the allocator with nothing said about where.
    fn array_with_dims_total(
        &mut self,
        dims: &[Expr],
        span: Span,
        outer: i64,
    ) -> Result<Value, RuntimeError> {
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
            n
        };
        let total = outer.saturating_mul(n);
        if total > MAX_ARRAY_ELEMENTS {
            return Err(RuntimeError::ArrayTooLarge {
                elements: total,
                limit: MAX_ARRAY_ELEMENTS,
                span: Some(span),
            });
        }
        let n = n as usize;
        if dims.len() == 1 {
            // An element nothing has written yet is an empty string, not 0 —
            // measured: `Local $e[3]` answers `VarGetType` "String", `IsNumber` 0,
            // `StringLen` 0, and the same holds for the slots `ReDim` grows.
            return Ok(Value::array(vec![Value::Str(String::new()); n]));
        }
        let mut rows = Vec::with_capacity(n);
        for _ in 0..n {
            rows.push(self.array_with_dims_total(&dims[1..], span, total)?);
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
                self.write_var_key(f.var.key(), item, VarScope::Auto, f.var.span);
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
            self.write_var_key(f.var.key(), v, VarScope::Auto, f.var.span);
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
    ///
    /// The evaluation runs with [`Runtime::strict_reads`] on, so a name that
    /// was never assigned is an [`RuntimeError::UndefinedVariable`] rather than
    /// `""`: this path is fed by a human at a prompt, and `print $contuer`
    /// answering `""` reads as a variable the program really holds. Script code
    /// keeps AutoIt's own behaviour, because that is what the script was
    /// written against.
    pub fn evaluate_expression(&mut self, source: &str, span: Span) -> Result<Value, RuntimeError> {
        let wrapped = format!("Func __au3_expr__()\n    Return {source}\nEndFunc\n");
        let prog = autoitv3_ast::parse(&wrapped).map_err(|e| RuntimeError::Unsupported {
            what: msg!("not an expression: {error} ({source})", error = e.msg, source = source),
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
                what: msg!("not an expression: {source}", source = source),
                span: Some(span),
            })?;
        let expr = body.iter().find_map(|stmt| match &stmt.kind {
            StmtKind::Return(Some(e)) => Some(e),
            _ => None,
        });
        let Some(expr) = expr else {
            return Err(RuntimeError::Unsupported {
                what: msg!("not an expression: {source}", source = source),
                span: Some(span),
            });
        };
        // Evaluated in the caller's frame: `evaluate_expression` deliberately
        // does not push one, so `$local` means what it means at the stop.
        // The flag is saved and restored rather than cleared, so a nested
        // evaluation (a `print` inside a breakpoint action, say) still leaves
        // the outer one strict when it returns.
        let strict = self.strict_reads;
        self.strict_reads = true;
        let out = self.eval_expr(expr);
        self.strict_reads = strict;
        out
    }

    pub fn execute_source(&mut self, src: &str, span: Span) -> Result<Value, RuntimeError> {
        let prog = autoitv3_ast::parse(src).map_err(|e| RuntimeError::Unsupported {
            what: msg!("Execute() parse error: {error}", error = e.msg),
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
                    self.funcs.insert(key, Rc::new(f.clone()));
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
                                    what: msg!("Execute() control flow {flow}", flow = format!("{other:?}")),
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
/// The function `#OnAutoItStartRegister` names, if the program carries the
/// directive.
///
/// AutoIt's preprocessor only ever reads directives at the top level, so this
/// walks the same way the preprocessor does ([`autoitv3_preproc::directives`]):
/// regions count, a directive inside a function body does not. The argument is
/// usually quoted (`"Prologue"`); the quotes are not part of the name.
fn start_register(prog: &Program) -> Option<String> {
    fn find(items: &[Item]) -> Option<String> {
        for item in items {
            match &item.kind {
                ItemKind::Directive(text) => {
                    let mut parts = text.trim().splitn(2, char::is_whitespace);
                    let name = parts.next().unwrap_or("");
                    if !name.eq_ignore_ascii_case("OnAutoItStartRegister") {
                        continue;
                    }
                    let argument = parts.next().unwrap_or("").trim().trim_matches('"').trim();
                    if !argument.is_empty() {
                        return Some(argument.to_string());
                    }
                }
                ItemKind::Region(region) => {
                    if let Some(found) = find(&region.items) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    find(&prog.items)
}

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

/// Quote one argument so `$CmdLineRaw` parses back into the same argument.
fn quote_cmdline_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        if c == '"' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// Days since 1970-01-01 to `(year, month, day)`, proleptic Gregorian.
///
/// Howard Hinnant's `civil_from_days`, which avoids both a timezone database
/// and any dependency.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Variable names are case-insensitive in AutoIt and stored without the `$`.
/// Bind `value` under `key`, reusing the stored key when it exists.
fn bind_var(vars: &mut HashMap<String, Value>, key: &str, value: Value) {
    match vars.get_mut(key) {
        Some(slot) => *slot = value,
        None => {
            vars.insert(key.to_string(), value);
        }
    }
}

fn var_key(name: &str) -> String {
    name.trim_start_matches('$').to_ascii_lowercase()
}

/// One subscript step: `base[key]`.
///
/// Shared by reads (`index_value`), writes (`assign_index`) and the `ByRef`
/// write-back path, so a `$a[$i]` read, a `$a[$i] =` write and a `Foo($a[$i])`
/// copy-out all resolve the same way. A string is not indexable and yields `""`,
/// which is what AutoIt does.
fn index_step(base: &Value, key: &Value, span: Span) -> Result<Value, RuntimeError> {
    match base {
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
            Ok(arr[i as usize].clone())
        }
        Value::Map(m) => {
            let k = MapKey::from_value(key);
            Ok(m.borrow().get(&k).cloned().unwrap_or(Value::Null))
        }
        Value::Str(_) => Ok(Value::Str(String::new())),
        Value::Null => Ok(Value::Null),
        other => Err(RuntimeError::Type {
            expected: "Array or Map",
            got: other.type_name().to_string(),
            span: Some(span),
        }),
    }
}

/// [`index_step`] over already-evaluated subscripts.
///
/// The `ByRef` call path evaluates subscripts once and then needs the value
/// *and* the path back, so it cannot go through `index_value`, which evaluates
/// as it walks.
fn index_by_keys(base: &Value, keys: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let mut cur = base.clone();
    for key in keys {
        cur = index_step(&cur, key, span)?;
    }
    Ok(cur)
}

/// Store `value` under `key` of `container`, which must be an array or a map.
fn store_index(
    container: &Value,
    key: &Value,
    value: Value,
    span: Span,
) -> Result<(), RuntimeError> {
    match container {
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
            m.borrow_mut().insert(MapKey::from_value(key), value);
            Ok(())
        }
        other => Err(RuntimeError::Type {
            expected: "Array or Map",
            got: other.type_name().to_string(),
            span: Some(span),
        }),
    }
}

/// [`store_index`] along an already-evaluated path: the write-back of a `ByRef`
/// argument like `$a[$i][$j]`, whose subscripts the call site resolved.
fn store_index_path(
    base: &Value,
    keys: &[Value],
    value: Value,
    span: Span,
) -> Result<(), RuntimeError> {
    let Some((last, leading)) = keys.split_last() else { return Ok(()) };
    let mut container = base.clone();
    for key in leading {
        container = index_step(&container, key, span)?;
    }
    store_index(&container, last, value, span)
}

/// Where a `ByRef` parameter copies its final value back to.
///
/// The callee is handed a *copy* of the argument, so the write has to be
/// replayed at the call site. Except for [`ByRefTarget::Elem`], which holds a
/// shared container, this is a borrow of the call-site expression: no name is
/// copied, and an ordinary call pays nothing for the mechanism.
enum ByRefTarget<'a> {
    /// The caller's variable, as the call site named it.
    Caller(&'a str),
    /// An element of a container the caller shares: `$a[$i]`, `$m["k"]`.
    ///
    /// The subscripts were evaluated once at the call site and are replayed
    /// here, because the array is `Rc`-shared — writing through it is visible to
    /// the caller with no copy-out at all.
    Elem { base: Value, keys: Vec<Value> },
    /// No call site to read (a callback, the public `call_*` API): the
    /// parameter name stands in, the best a positional caller can offer.
    ParamName,
    /// The argument was not a variable — a literal, an expression, `$s[0]` on a
    /// string — so there is nothing to copy back to. AutoIt rejects literals.
    Nothing,
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

/// Whether evaluating `e` can change a variable.
///
/// Only a call can: AutoIt's assignment operators are statement-level, so a
/// `=` inside an expression is the comparison, and every other operator reads
/// its operands and returns a value. `Call`/`IndexCall`/`MethodCall` cover the
/// ways a script reaches a function — including `Execute`, `Eval` and `Assign`,
/// which are builtins. `Member` is treated as impure because a property getter
/// may run code.
fn expr_is_pure(e: &Expr) -> bool {
    use BinaryOp::*;
    match &e.kind {
        ExprKind::Lit(_) | ExprKind::Macro(_) | ExprKind::Ident(_) | ExprKind::WithSubject => true,
        ExprKind::Call(_)
        | ExprKind::IndexCall(..)
        | ExprKind::MethodCall(..)
        | ExprKind::Member(..) => false,
        ExprKind::Var(v) => v.indices.iter().all(expr_is_pure),
        ExprKind::Unary(_, a) | ExprKind::Paren(a) => expr_is_pure(a),
        // An assignment parses as a statement, so this cannot turn up inside an
        // expression — but if it ever does, it is not pure.
        ExprKind::Binary(op, a, b) => {
            !matches!(
                op,
                Assign | PlusAssign | MinusAssign | StarAssign | SlashAssign | CaretAssign
                    | AmpAssign
            ) && expr_is_pure(a)
                && expr_is_pure(b)
        }
        ExprKind::Ternary(c, a, b) => expr_is_pure(c) && expr_is_pure(a) && expr_is_pure(b),
        ExprKind::ArrayLit(items) => items.iter().all(expr_is_pure),
        ExprKind::Subscript(base, indices) => {
            expr_is_pure(base) && indices.iter().all(expr_is_pure)
        }
    }
}

/// Append `text` to a value that is about to become a string, coercing it the
/// way `&` does when it is not one already.
fn append_text(slot: &mut Value, text: &str) {
    match slot {
        Value::Str(s) => s.push_str(text),
        other => {
            let prefix = other.to_autoit_string();
            *other = Value::Str(prefix + text);
        }
    }
}

/// Numeric / string arithmetic shared by binary operators and compound assigns.
fn apply_arith(l: &Value, r: &Value, op: &BinaryOp) -> Value {
    use BinaryOp::*;
    match op {
        // Two binaries concatenate byte-wise, not as their `0x…` spellings:
        // the crypto UDFs build their HMAC input as `$iv & $ciphertext`.
        Concat => match (l, r) {
            (Value::Binary(a), Value::Binary(b)) => {
                let mut out = Vec::with_capacity(a.len() + b.len());
                out.extend_from_slice(a);
                out.extend_from_slice(b);
                Value::Binary(Rc::new(out))
            }
            _ => Value::Str(format!("{}{}", l.to_autoit_string(), r.to_autoit_string())),
        },
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
    options: &'a HashMap<String, Value>,
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

    fn option(&self, name: &str) -> Option<Value> {
        self.options.get(&var_key(name)).cloned()
    }
}
/// Copy a literal's values into a declared array, position by position.
///
/// A literal with more values in a dimension than the declaration has room for is
/// the error AutoIt raises there — measured: both `Local $c[2] = [1, 2, 3]` and a
/// 3x3 literal for a 2x2 declaration stop with "Array variable has incorrect number
/// of subscripts or subscript dimension range exceeded".
fn fill_from_literal(
    target: &Value,
    literals: &Value,
    span: Span,
) -> Result<(), RuntimeError> {
    let (Value::Array(target), Value::Array(literals)) = (target, literals) else {
        return Ok(());
    };
    let literals = literals.borrow();
    let mut target = target.borrow_mut();
    if literals.len() > target.len() {
        return Err(RuntimeError::IndexOutOfBounds {
            index: target.len() as i64,
            len: target.len(),
            span: Some(span),
        });
    }
    for (slot, value) in target.iter_mut().zip(literals.iter()) {
        if matches!(slot, Value::Array(_)) {
            fill_from_literal(slot, value, span)?;
        } else {
            *slot = value.clone();
        }
    }
    Ok(())
}

/// True for one `[]` bracket — the parser records it as a `Null` literal
/// dimension.
fn dim_is_empty(dim: &Expr) -> bool {
    matches!(dim.kind, ExprKind::Lit(Lit { kind: LitKind::Null, .. }))
}

/// The extents an initializer offers, one per nesting level.
///
/// `[[1, 2], [3, 4]]` offers two rows of up to two cells, so `[2, 2]`: a short
/// row does not lower the count (measured — `Local $a[][3] = [[1,2,3],[4]]` is
/// 2x3), and a level the initializer does not reach counts as 0 (measured —
/// `Local $a[] = 5` is an empty array). The count of a level is the longest
/// run at that level, which is what makes the literal *rectangular*: rows are
/// the sub-arrays, and a cell a short row does not fill is an empty string.
fn literal_extents(value: &Value, dims: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(dims);
    let mut current = match value {
        Value::Array(items) => Some(Rc::clone(items)),
        _ => None,
    };
    for _ in 0..dims {
        let Some(items) = current.take() else {
            out.push(0);
            continue;
        };
        let items = items.borrow();
        out.push(items.len());
        current = items
            .iter()
            .filter_map(|cell| match cell {
                Value::Array(row) => Some(row),
                _ => None,
            })
            .max_by_key(|row| row.borrow().len())
            .map(Rc::clone);
    }
    out
}
