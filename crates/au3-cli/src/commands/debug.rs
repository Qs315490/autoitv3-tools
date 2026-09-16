//! `au3 debug <FILE> [-c CMD]...` — an interactive debugger shell.
//!
//! The shape is gdb's, because that is the shape the runtime's debug interface
//! was built for: a session holds a loaded program but does not run it, `run`
//! starts (or restarts) it, and execution stops at breakpoints and steps, where
//! a prompt lets you look around before resuming.
//!
//! ```text
//! $ au3 debug a.au3
//! (au3) break 68
//! Breakpoint 1 at line 68
//! (au3) run
//! Breakpoint 1, line 68
//!     68  Global Const $string_table = f563()
//! (au3:68:1) print $name_table[1]
//! "CryptDecrypt"
//! (au3:68:1) next
//! ...
//! ```
//!
//! ## How a stop works
//!
//! [`Debugger::on_stop`] is called *from inside the interpreter*, with the Rust
//! stack still live, so the prompt runs there and returns when the user says
//! `continue`. Stepping is this module's own bookkeeping — see [`StepMode`] —
//! rather than something the interpreter has to model.
//!
//! ## One command source
//!
//! Commands come from a single queue: the `-c` flags first, then stdin. The
//! outer loop and the prompt at a breakpoint draw from the same queue, so
//! `-c run -c next -c 'print $x' -c quit` means what it looks like — `run`
//! stops, and the remaining commands are consumed by the prompt.
//!
//! ## Watching a GUI script
//!
//! The emulated GUI answers its 165 functions on every host; what draws them is
//! the backend, and by default that is the platform's own — real Win32 controls
//! on Windows, nothing at all elsewhere. `--gui headless` forces the in-memory
//! model for a session that only has to be stepped through, and `--gui window`
//! (build with `--features gui-window`) puts the session under an eframe window
//! instead: winit takes the main thread, so the shell, the interpreter and the
//! prompt all move to `LiveBackend`'s worker, and stdin keeps working there.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::rc::Rc;

use autoitv3_ast::span::Span;
use autoitv3_ast::Program;
use autoitv3_runtime::debug::{Breakpoint, DebugAction, DebugHost, Debugger, FrameInfo, StopReason};
use autoitv3_runtime::RuntimeError;
use autoitv3_runtime::Runtime;
use clap::Args;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Result as RustyResult};

use crate::args::{
    CliError, CliResult, CompiledArgs, EffectArgs, GuiMode, IncludeArgs, ProfileArgs, StepArgs,
    WinEmuArgs, load_input_included,
};

use crate::output::format_value;
use std::path::Path;

/// Arguments for `au3 debug`.
#[derive(Args, Debug, Clone)]
pub struct DebugArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Command to run at startup; repeat for a whole session.
    ///
    /// Commands are accepted wherever a prompt would appear, including at a
    /// breakpoint, so `-c run -c next -c quit` walks the script. Several
    /// commands may share one `-c` if they are separated by `;`
    /// (`-c "break 11; run; print $i"`); a `;` inside a "quoted string" is left
    /// alone.
    #[arg(short = 'c', long = "command", value_name = "CMD")]
    pub commands: Vec<String>,

    /// File of commands to run at startup, one per line; repeat for more.
    ///
    /// Blank lines and lines starting with `#` are ignored, and `;` separates
    /// several commands on one line. These run before any `-c` command, and the
    /// session stays interactive afterwards — `-x setup.au3dbg` is how you get
    /// "load my usual breakpoints, then hand me the prompt".
    #[arg(short = 'x', long = "command-file", value_name = "FILE")]
    pub command_files: Vec<String>,

    /// Do not stop when a statement fails with an uncaught error
    ///
    /// The default is to stop where the error was raised — the frame that
    /// raised it is still on the stack, so it can be inspected like a
    /// breakpoint. Use `catch off` at the prompt to change it mid-session.
    #[arg(long = "no-catch")]
    pub no_catch: bool,

    /// Stop on the first statement the script executes, as if `step` had been
    /// typed before `run`
    #[arg(long)]
    pub stop_at_start: bool,

    /// Execution semantics (see `ProfileArgs`).
    #[command(flatten)]
    pub profile: ProfileArgs,

    /// Per-effect allow/deny overrides (see `EffectArgs`).
    #[command(flatten)]
    pub effects: EffectArgs,

    /// Interpreter step budget (see `StepArgs`).
    #[command(flatten)]
    pub steps: StepArgs,

    /// `@Compiled` selection (see `CompiledArgs`).
    #[command(flatten)]
    pub compiled: CompiledArgs,

    /// GUI backend: `auto` (the default) is the platform's own — real Win32
    /// controls on Windows, nothing drawn elsewhere; `headless` answers the GUI
    /// functions without drawing anything on any host; `window` runs the session
    /// under an eframe window (needs a build with the `gui-window` feature)
    #[arg(long = "gui", value_name = "MODE", default_value = "auto")]
    pub gui: GuiMode,

    /// `#include` search path (see `IncludeArgs`).
    #[command(flatten)]
    pub includes: IncludeArgs,

    #[command(flatten)]
    pub win: WinEmuArgs,
}

/// Builds the GUI backend a fresh runtime gets.
///
/// The session builds a runtime more than once — every `run` asks for a new one
/// — but a backend is handed to the platform stack by value, so the session
/// keeps a factory instead of a backend. `--gui window`'s factory hands out
/// clones of one `LiveBackend`, which all describe the same window; the other
/// modes build a fresh backend each time, which is just as good because they own
/// no window.
type GuiFactory = Box<dyn Fn() -> Box<dyn autoitv3_platform::winemu::GuiBackend>>;

/// Entry point for the `debug` subcommand.
///
/// The session runs on this thread by default. `--gui window` has to hand the
/// main thread to the window loop (winit insists on it), so it moves the whole
/// session — shell included — onto `LiveBackend`'s worker instead.
pub fn run(args: &DebugArgs) -> CliResult<()> {
    match args.gui {
        // No factory: the platform stack keeps its own backend, which is the
        // native Win32 one on Windows.
        GuiMode::Auto => session(args, None),
        GuiMode::Headless => session(
            args,
            Some(Box::new(|| {
                Box::new(autoitv3_platform::winemu::HeadlessBackend::new())
            })),
        ),
        GuiMode::Window => run_windowed(args),
    }
}

/// `--gui window`: run the whole debug session under a real window.
///
/// The window owns the main thread, so everything below — shell, interpreter
/// and all — happens on the worker thread `LiveBackend::run` starts. The shell
/// still reads stdin there, which is what makes the prompt usable while the
/// window is on screen.
#[cfg(feature = "gui-window")]
fn run_windowed(args: &DebugArgs) -> CliResult<()> {
    let title = Path::new(&args.input)
        .file_name()
        .map(|name| format!("au3 debug — {}", name.to_string_lossy()))
        .unwrap_or_else(|| "au3 debug".to_string());
    let owned = args.clone();
    autoitv3_gui_egui::LiveBackend::new(title)
        .run(move |backend| {
            let factory: GuiFactory = Box::new(move || Box::new(backend.clone()));
            if let Err(e) = session(&owned, Some(factory)) {
                eprintln!("error: {}", e.message);
            }
        })
        .map_err(|e| CliError::failure(format!("opening the GUI window failed: {e}")))
}

/// `--gui window` without the feature: say how to get one.
#[cfg(not(feature = "gui-window"))]
fn run_windowed(_args: &DebugArgs) -> CliResult<()> {
    Err(CliError::failure(
        "--gui window needs a build with the `gui-window` feature \
         (cargo build --release -p au3-cli --features gui-window)",
    ))
}

/// The debug session: command files, shell and run loop.
///
/// `gui` is the backend factory when a mode overrode the platform's own
/// (see [`GuiFactory`]); `None` leaves the choice to the platform.
fn session(args: &DebugArgs, gui: Option<GuiFactory>) -> CliResult<()> {
    let input = load_input_included(&args.input, &args.includes)?;
    let source = input.source;
    let prog = input.program;
    let resource_module = input.resource_module;

    // A debug session is interactive, and an elevated copy would be a second
    // process without this shell's stdin — the session would end at the first
    // prompt. Say what was not done instead of silently stepping through a
    // script that asked for rights.
    if crate::elevate::is_required(&prog) {
        eprintln!(
            "note: #RequireAdmin: this script wants administrator rights; \
             the debugger does not elevate — run it from an elevated shell to match"
        );
    }

    let mut file_commands = Vec::new();
    for path in &args.command_files {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CliError::io(format!("cannot read command file {path}: {e}")))?;
        for line in text.lines() {
            file_commands.extend(split_commands(line));
        }
    }

    let shell = Rc::new(RefCell::new(Shell::new(
        args.input.clone(),
        &source,
        args,
        file_commands,
    )));
    let mut rt = build_runtime(&prog, args, shell.clone(), resource_module.as_deref(), &gui);

    // The outer loop. A `Resume` here means "start the script body"; the same
    // answer at a breakpoint means "give control back to the interpreter",
    // which is what [`Shell::prompt_loop`] does with it.
    while !shell.borrow().finished {
        // The borrow must end before the run: the runtime calls back into the
        // shell, and holding it here would silence every callback.
        let Some(cmd) = shell.borrow_mut().next_logical(Some(&mut rt)) else {
            break;
        };
        if shell.borrow_mut().execute(&cmd, &mut rt) != Outcome::Resume {
            continue;
        }
        // Start a run. `run` typed at a stop asks for a fresh one: the request
        // unwinds the current run first, which is what `take_restart` reports.
        loop {
            rt = build_runtime(&prog, args, shell.clone(), resource_module.as_deref(), &gui);
            shell.borrow_mut().restore_breakpoints(&mut rt);
            shell.borrow_mut().begin_run();
            let result = rt.run_script();
            if shell.borrow_mut().take_restart() {
                continue;
            }
            shell.borrow_mut().report_run(&result);
            break;
        }
    }
    Ok(())
}

/// Build a runtime with the platform, profile and shell this session uses.
fn build_runtime(
    prog: &Program,
    args: &DebugArgs,
    shell: Rc<RefCell<Shell>>,
    resource_module: Option<&Path>,
    gui: &Option<GuiFactory>,
) -> Runtime {
    let mut rt = Runtime::with_program(prog);
    // A factory, not a backend: each runtime gets its own handle to the window.
    let platform = args.win.platform(
        Some(Path::new(&args.input)),
        resource_module,
        gui.as_ref().map(|make| make()),
    );
    match platform {
        Ok(platform) => rt.set_platform(platform),
        Err(e) => eprintln!("warning: {}", e.message),
    }
    // The build the script came out of answered `@Compiled = 1`; the flags
    // override that when a `.au3` is being compared against its build.
    rt.set_compiled(args.compiled.resolve(resource_module.is_some()));
    rt.set_max_steps(args.steps.max_steps);
    match args.effects.apply(args.profile.profile()) {
        Ok(p) => rt.set_profile(p),
        Err(e) => eprintln!("warning: {}", e.message),
    }
    rt.set_debugger(Box::new(SharedShell(shell)));
    rt
}

/// The debugger handle the runtime owns.
///
/// The shell lives behind an `Rc` so the driver loop can reach it too.
/// `try_borrow_mut` rather than `borrow_mut` is deliberate: a command such as
/// `print` evaluates program source, which re-enters the interpreter and offers
/// it statements. That is not a stop, so the nested offer is answered with
/// `Continue` instead of panicking on a second borrow.
struct SharedShell(Rc<RefCell<Shell>>);

impl Debugger for SharedShell {
    fn on_statement(&mut self, span: Span, depth: usize, host: &mut dyn DebugHost) -> DebugAction {
        match self.0.try_borrow_mut() {
            Ok(mut shell) => shell.on_statement(span, depth, host),
            Err(_) => DebugAction::Continue,
        }
    }

    fn on_error(&mut self, error: &RuntimeError, span: Option<Span>, host: &mut dyn DebugHost) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_error(error, span, host);
        }
    }

    fn on_stop(&mut self, reason: &StopReason, host: &mut dyn DebugHost) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_stop(reason, host);
        }
    }

    fn on_breakpoint_action(&mut self, bp: &Breakpoint, host: &mut dyn DebugHost) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_breakpoint_action(bp, host);
        }
    }

    fn on_variable_write(&mut self, name: &str, value: &autoitv3_runtime::Value) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_variable_write(name, value);
        }
    }

    fn on_call_enter(
        &mut self,
        name: &str,
        args: &[autoitv3_runtime::Value],
    ) -> DebugAction {
        match self.0.try_borrow_mut() {
            Ok(mut shell) => shell.on_call_enter(name, args),
            Err(_) => DebugAction::Continue,
        }
    }

    fn on_call_exit(&mut self, name: &str, result: Option<&autoitv3_runtime::Value>) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_call_exit(name, result);
        }
    }

    fn on_builtin_call(
        &mut self,
        name: &str,
        args: &[autoitv3_runtime::Value],
    ) -> DebugAction {
        match self.0.try_borrow_mut() {
            Ok(mut shell) => shell.on_builtin_call(name, args),
            Err(_) => DebugAction::Continue,
        }
    }
}

/// Every command word offered at the start of a line.
///
/// Kept in step with the parser in [`Shell::execute`]; aliases are included so
/// completion matches what can actually be typed.
const COMMAND_WORDS: &[&str] = &[
    "run", "restart", "continue", "c", "step", "s", "next", "n", "finish", "fin", "until", "u",
    "untilcall", "untilc", "uc", "untilret", "untilr", "ur", "untilgui", "gui", "stopat", "sa",
    "break",
    "b", "tbreak", "tb",
    "jmp", "j", "frame", "f", "up", "down",
    "delete", "d", "del", "enable", "disable", "print", "p", "set", "info", "i", "backtrace",
    "bt", "where", "w", "list", "l", "eval", "watch", "unwatch", "ignore", "commands",
    "nostop", "stop", "catch", "source", "trace", "help", "h", "?", "quit", "q", "exit",
];

/// `info <topic>` arguments.
const INFO_TOPICS: &[&str] = &["breakpoints", "locals", "globals", "functions", "frame"];

/// Completion candidates, snapshotted from the live session before each prompt
/// (the completer runs under rustyline's borrow, so it cannot reach the shell).
#[derive(Default)]
struct CompletionData {
    commands: Vec<String>,
    functions: Vec<String>,
    builtins: Vec<String>,
    macros: Vec<String>,
    globals: Vec<String>,
    breakpoints: Vec<String>,
}

/// The debug prompt's editor.
type LineEditor = Editor<DebugCompleter, DefaultHistory>;

/// Tab completion for the debugger prompt.
#[derive(Default)]
struct DebugCompleter {
    data: CompletionData,
}

impl Completer for DebugCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> RustyResult<(usize, Vec<Pair>)> {
        let (start, word, head) = current_word(line, pos);
        Ok((start, self.data.candidates(head.as_deref(), word)))
    }
}

impl Hinter for DebugCompleter {
    type Hint = String;
}
impl Highlighter for DebugCompleter {}
impl Validator for DebugCompleter {}
impl rustyline::Helper for DebugCompleter {}

impl CompletionData {
    /// Candidates for the word being completed; `head` is the command word
    /// already typed, or `None` when the cursor is on the first word.
    fn candidates(&self, head: Option<&str>, word: &str) -> Vec<Pair> {
        let mut pool: Vec<String> = Vec::new();
        match head {
            None => pool.extend(self.commands.iter().cloned()),
            Some(head) => match head.to_ascii_lowercase().as_str() {
                "break" | "b" | "tbreak" | "tb" | "until" | "u" | "jmp" | "j" => {
                    pool.extend(self.functions.iter().cloned())
                }
                "untilcall" | "untilc" | "uc" | "untilret" | "untilr" | "ur" | "stopat" | "sa" => {
                    pool.extend(self.builtins.iter().cloned());
                    pool.extend(self.functions.iter().cloned());
                }
                "print" | "p" | "set" | "eval" | "watch" => {
                    pool.extend(self.globals.iter().cloned());
                    pool.extend(self.macros.iter().cloned());
                }
                "help" | "h" | "?" => pool.extend(self.commands.iter().cloned()),
                "info" | "i" => pool.extend(INFO_TOPICS.iter().map(|s| s.to_string())),
                "delete" | "d" | "del" | "enable" | "disable" | "nostop" | "stop"
                | "ignore" | "commands" | "unwatch" => {
                    pool.extend(self.breakpoints.iter().cloned())
                }
                _ => {}
            },
        }
        let lower = word.to_ascii_lowercase();
        pool.retain(|c| c.to_ascii_lowercase().starts_with(&lower));
        pool.sort();
        pool.dedup();
        pool.into_iter()
            .map(|c| Pair {
                display: c.clone(),
                replacement: c,
            })
            .collect()
    }
}

/// The word under the cursor: its start byte, the partial text, and the
/// command word already typed on the line (`None` when completing the first).
fn current_word(line: &str, pos: usize) -> (usize, &str, Option<String>) {
    let before = &line[..pos];
    let start = before
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0);
    let word = &line[start..pos];
    let head = line[..start]
        .split_whitespace()
        .next()
        .map(|s| s.to_string());
    (start, word, head)
}

/// What a command asked the caller to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Keep prompting here.
    Stay,
    /// Hand control back — to the interpreter at a stop, or by starting the
    /// script from the outer loop.
    Resume,
    /// Leave the session.
    Quit,
}

/// How execution should behave after the current stop ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepMode {
    /// Run until a breakpoint fires.
    Run,
    /// Stop after this many statements, wherever they are.
    Step(u64),
    /// Stop after this many statements in this frame or a shallower one.
    Over(usize, u64),
    /// Stop once the current frame has been left.
    Out(usize),
}

/// A breakpoint edit a command recorded, waiting for the host to apply it.
///
/// Commands that read the program take the host directly; the ones that *edit*
/// it are written as a small pending action so that the command parser stays
/// free of the host, and applied in one place at the end of [`Shell::execute`].
enum Edit {
    Add(BpSpec),
    Remove(Option<u32>),
    Enable(u32, bool),
}

/// Everything `break`/`jmp` can put on one breakpoint.
struct BpSpec {
    line: u32,
    condition: Option<String>,
    skip: u64,
    every: u64,
    stop: bool,
    actions: Vec<String>,
    /// Display label for function breakpoints (`"func Foo"`).
    label: Option<String>,
    /// `jmp` target: delete the breakpoint once it fires.
    temp: bool,
}

impl BpSpec {
    fn line(line: u32) -> Self {
        Self {
            line,
            condition: None,
            skip: 0,
            every: 1,
            stop: true,
            actions: Vec::new(),
            label: None,
            temp: false,
        }
    }
}

/// One `watch` expression and the value it had when last observed
/// (the `Debug` rendering; `None` = no baseline yet).
struct Watch {
    id: u32,
    expr: String,
    last: Option<String>,
}

/// The interactive session: the debugger, the command loop, and the source.
struct Shell {
    /// The file being debugged, for prompts and `list`.
    script: String,
    /// Its lines, 1-based when indexed as `lines[line - 1]`.
    lines: Vec<String>,
    /// `-c` commands, then stdin.
    queue: VecDeque<String>,
    /// Whether to draw a prompt. Commands are read from stdin either way — a
    /// pipe is a perfectly good way to drive a session — but a prompt would
    /// only pollute the output of one.
    show_prompts: bool,
    /// Reached EOF on stdin, or the user typed `quit`.
    finished: bool,
    /// Step policy for the next statement.
    step: StepMode,
    /// Stop on the first statement after the next `run`.
    stop_at_start: bool,
    /// The statement the interpreter is at, and the frame depth it is at.
    ///
    /// Updated on *every* statement, not only on a stop: a breakpoint fires
    /// without the stepping logic asking for one, and `list`, `next` and the
    /// prompt all need to know where they are either way.
    current: Option<(Span, usize)>,
    /// Whether execution is suspended at a prompt.
    paused: bool,
    /// Echo every statement as it runs.
    tracing: bool,
    /// Only trace statements at this depth or shallower (`trace depth N`).
    trace_depth: Option<usize>,
    /// Suppress tracing while any of these functions is on the stack
    /// (`trace skip Func`), so a hot helper does not flood the output.
    trace_skip: Vec<String>,
    /// Script-defined functions currently on the stack, innermost last.
    ///
    /// Maintained from the call hooks so `trace skip` does not have to snapshot
    /// the whole stack on every statement.
    call_stack: Vec<String>,
    /// `untilcall <name>`: stop *before* the next call to this builtin/function
    /// (lower-case). One-shot, like `tbreak`.
    until_call: Option<String>,
    /// `untilret <name>`: stop at the statement after the next call to this
    /// builtin/function returns (lower-case). One-shot.
    until_ret: Option<String>,
    /// `frame <n>`/`up`/`down`: the selected frame, numbered the way gdb
    /// numbers a stack (0 = innermost). `None` is the innermost frame, which is
    /// also what every stop resets to.
    selected_frame: Option<usize>,
    /// `stopat <name>...`: stop *before* every call to any of these
    /// builtin/functions (lower-case), so their arguments — a dialog's text, a
    /// DLL name — can be read without the call running. Several targets may be
    /// given and they accumulate, one `stopat` command per target, the way
    /// gdb's target-taking catchpoints work (`catch syscall <name>`,
    /// `catch load <lib>`). gdb's bare `catch` is the whole family of event
    /// catchpoints; its error-side member, `catch throw`, is what this
    /// debugger's `catch on|off` corresponds to. `stopat off` clears them all.
    stop_at: Vec<String>,
    /// The call a catchpoint is stopping on, formatted for the banner.
    caught_call: Option<String>,
    /// The `untilcall` target has been seen; stop at the next statement.
    until_hit: bool,
    /// Line editor with completion. Present only when stdin is a terminal;
    /// pipes, `-c` and command files keep the plain reader.
    editor: Option<LineEditor>,
    /// Stop where an uncaught error was raised.
    catching: bool,
    /// An error was already shown at its source, so the end-of-run report
    /// should not repeat it.
    reported_error: bool,
    /// A breakpoint edit for [`Shell::flush_edits`] to apply.
    pending: Option<Edit>,
    /// `run` was typed at a stop, so the current run should unwind and start
    /// over rather than simply resuming.
    restart: bool,
    /// Breakpoint specs remembered so `run` can restart with them intact.
    saved_breakpoints: Vec<(Breakpoint, Option<String>)>,
    /// Active `watch` expressions.
    watches: Vec<Watch>,
    next_watch: u32,
    /// `jmp` targets: temporary breakpoint ids, deleted when they fire.
    jmp_pending: Vec<u32>,
    /// Display labels for function breakpoints, by breakpoint id.
    bp_labels: Vec<(u32, String)>,
    /// A variable was written while running: re-observe the watches at the
    /// next statement (`on_variable_write` has no host, so the check happens
    /// in [`Shell::on_statement`], which does).
    pending_watch_check: bool,
}

impl Shell {
    fn new(script: String, source: &str, args: &DebugArgs, file_commands: Vec<String>) -> Self {
        // Command files first, then `-c`, then stdin: every source feeds the
        // one queue, so the order they appear in is the order they run.
        let mut queue: VecDeque<String> = file_commands.into();
        for raw in &args.commands {
            queue.extend(split_commands(raw));
        }
        let show_prompts = std::io::stdin().is_terminal();
        let editor = if show_prompts {
            LineEditor::new()
                .ok()
                .map(|mut editor| {
                    editor.set_helper(Some(DebugCompleter::default()));
                    editor
                })
        } else {
            None
        };
        Self {
            script,
            lines: source.lines().map(|l| l.to_string()).collect(),
            queue,
            show_prompts,
            finished: false,
            step: StepMode::Run,
            stop_at_start: args.stop_at_start,
            current: None,
            paused: false,
            tracing: false,
            trace_depth: None,
            trace_skip: Vec::new(),
            call_stack: Vec::new(),
            until_call: None,
            until_ret: None,
            selected_frame: None,
            stop_at: Vec::new(),
            caught_call: None,
            until_hit: false,
            editor,
            catching: !args.no_catch,
            reported_error: false,
            pending: None,
            restart: false,
            saved_breakpoints: Vec::new(),
            watches: Vec::new(),
            next_watch: 1,
            jmp_pending: Vec::new(),
            bp_labels: Vec::new(),
            pending_watch_check: false,
        }
    }

    /// Whether a `run` at a stop asked for the session to start over.
    fn take_restart(&mut self) -> bool {
        std::mem::take(&mut self.restart)
    }

    /// Re-apply the breakpoints after a restart replaced the runtime.
    ///
    /// The specs are read off the old host by the caller, which is why this
    /// takes the new one and the old list.
    fn restore_breakpoints(&mut self, host: &mut dyn DebugHost) {
        let specs = std::mem::take(&mut self.saved_breakpoints);
        for (bp, label) in specs {
            let id = host.add_breakpoint_full(
                bp.line,
                bp.condition.clone(),
                bp.skip_remaining,
                bp.every,
                bp.stop,
                bp.actions.clone(),
            );
            // `add_breakpoint_full` always creates an enabled breakpoint;
            // re-apply the saved enabled state so a disabled breakpoint does
            // not come back armed after a restart.
            host.set_breakpoint_enabled(id, bp.enabled);
            if let Some(label) = label {
                self.bp_labels.push((id, label));
            }
        }
        // Watch baselines are stale after a restart: re-observe from scratch.
        for w in &mut self.watches {
            w.last = None;
        }
    }

    /// Note that a run is starting.
    fn begin_run(&mut self) {
        self.current = None;
        self.paused = false;
        self.restart = false;
        // `jmp_pending` survives: a `jmp` typed before `run` is exactly the
        // case where the target must still be pending when the run starts.
        //
        // A `step`/`next` typed before the run is a request to stop early and
        // must survive too; `--stop-at-start` is only the fallback for when
        // nothing asked to step.
        if matches!(self.step, StepMode::Run) && self.stop_at_start {
            self.step = StepMode::Step(1);
        }
    }

    /// Report how the script body ended.
    fn report_run(&mut self, outcome: &Result<autoitv3_runtime::Flow, autoitv3_runtime::RuntimeError>) {
        // The run is over: a step budget it did not get to spend must not leak
        // into the next one.
        self.step = StepMode::Run;
        if self.finished {
            return;
        }
        match outcome {
            Ok(_) => println!("[script finished]"),
            // A caught error was already printed where it was raised.
            Err(_) if std::mem::take(&mut self.reported_error) => {}
            Err(e) => println!("[script stopped: {e}]"),
        }
    }

    // ----- command source -----

    /// The next command: a queued one, then a line from stdin.
    ///
    /// On a terminal the line is read through rustyline, so tab completion and
    /// history work; a pipe uses the plain reader. `host` is only needed to
    /// offer live candidates (function names, variables, breakpoints).
    fn next_command(&mut self, host: Option<&dyn DebugHost>) -> Option<String> {
        if self.finished {
            return None;
        }
        if let Some(cmd) = self.queue.pop_front() {
            if self.show_prompts {
                println!("{}{cmd}", self.prompt());
            }
            return Some(cmd);
        }
        if self.show_prompts {
            let prompt = self.prompt();
            let data = self.completion_data(host);
            if let Some(editor) = self.editor.as_mut() {
                if let Some(helper) = editor.helper_mut() {
                    helper.data = data;
                }
                loop {
                    match editor.readline(&prompt) {
                        Ok(line) => {
                            // rustyline only records history when
                            // `Config::auto_add_history` is set (off by
                            // default), so keep the line ourselves or Up
                            // would recall nothing.
                            let _ = editor.add_history_entry(line.as_str());
                            return Some(line);
                        }
                        // Ctrl-C clears the line and keeps the session, as at
                        // any other prompt.
                        Err(ReadlineError::Interrupted) => {
                            println!("^C");
                            continue;
                        }
                        Err(ReadlineError::Eof) => {
                            self.finished = true;
                            return None;
                        }
                        // No usable terminal after all: fall through to the
                        // plain reader below.
                        Err(_) => break,
                    }
                }
            }
            print!("{prompt}");
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => {
                if self.show_prompts {
                    println!();
                }
                self.finished = true;
                None
            }
            Ok(_) => Some(line),
        }
    }

    /// Snapshot the completion candidates from the live session.
    fn completion_data(&self, host: Option<&dyn DebugHost>) -> CompletionData {
        let mut functions = host.map(|h| h.function_names()).unwrap_or_default();
        functions.sort();
        CompletionData {
            commands: COMMAND_WORDS.iter().map(|s| s.to_string()).collect(),
            functions,
            builtins: autoitv3_runtime::vocab::FUNCTIONS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            macros: autoitv3_runtime::vocab::MACROS
                .iter()
                .map(|s| format!("@{s}"))
                .collect(),
            globals: host
                .map(|h| h.globals().into_iter().map(|(k, _)| format!("${k}")).collect())
                .unwrap_or_default(),
            breakpoints: host
                .map(|h| {
                    h.breakpoints()
                        .into_iter()
                        .map(|b| b.id.to_string())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The prompt, which shows where execution is stopped.
    fn prompt(&self) -> String {
        match self.current {
            Some((span, _)) if self.paused => {
                format!("(au3:{}:{}) ", span.start.line, span.start.col)
            }
            _ => "(au3) ".to_string(),
        }
    }

    // ----- command execution -----

    /// Run one command line.
    ///
    /// `Outcome::Resume` is interpreted by the caller: at a stop it means
    /// "return to the interpreter", at the top level "start the script".
    fn execute(&mut self, raw: &str, host: &mut dyn DebugHost) -> Outcome {
        // A bare Enter repeats the last interesting direction and resumes,
        // which is what gdb does.
        let line = if raw.trim().is_empty() { "step" } else { raw.trim() };
        let (word, rest) = split_command(line);
        let outcome = match word.as_str() {
            "h" | "help" | "?" => {
                self.print_help(rest.trim());
                Outcome::Stay
            }
            "q" | "quit" | "exit" => {
                self.finished = true;
                Outcome::Quit
            }
            "r" | "run" | "restart" => {
                // Stopped: start over rather than resume, the way gdb does.
                if self.paused {
                    self.restart = true;
                }
                Outcome::Resume
            }
            "c" | "cont" | "continue" => {
                self.step = StepMode::Run;
                Outcome::Resume
            }
            "s" | "step" => self.step_command("step", rest.trim()),
            "n" | "next" => self.step_command("next", rest.trim()),
            "fin" | "finish" => self.step_command("finish", rest.trim()),
            // gdb's `until` collapses into the one-shot breakpoint: with no
            // frame-boundary semantics it was a duplicate of `tbreak <line>`.
            "until" | "u" => self.tbreak_command(rest.trim(), host),
            // `untilcall` is the builtin counterpart: builtins have no entry
            // line, so it watches the resolved call instead.
            "untilcall" | "untilc" | "uc" => self.until_call_command(rest.trim()),
            "untilret" | "untilr" | "ur" => self.until_ret_command(rest.trim()),
            "stopat" | "sa" => self.stop_at_command(rest.trim()),
            "untilgui" | "gui" => self.until_call_command("GUICreate"),
            "b" | "break" => self.break_command(rest.trim(), host),
            "jmp" | "j" => self.jmp_command(rest.trim(), host),
            "tbreak" | "tb" => self.tbreak_command(rest.trim(), host),
            "eval" => self.eval_command(rest.trim(), host),
            "ignore" => self.ignore_command(rest.trim(), host),
            "commands" => self.commands_command(rest.trim(), host),
            "nostop" => self.stop_toggle_command(rest.trim(), false, host),
            "stop" => self.stop_toggle_command(rest.trim(), true, host),
            "watch" => self.watch_command(rest.trim(), host),
            "unwatch" => self.unwatch_command(rest.trim()),
            "d" | "del" | "delete" => self.delete_command(rest.trim()),
            "enable" => self.toggle_command(rest.trim(), true),
            "disable" => self.toggle_command(rest.trim(), false),
            "p" | "print" => {
                self.eval_and_print(rest.trim(), host);
                Outcome::Stay
            }
            "set" => {
                self.set_command(rest.trim(), host);
                Outcome::Stay
            }
            "i" | "info" => {
                self.info_command(rest.trim(), host);
                Outcome::Stay
            }
            "frame" | "f" => {
                self.frame_command(rest.trim(), host);
                Outcome::Stay
            }
            "up" | "down" => {
                self.move_frame_command(word.as_str(), rest.trim(), host);
                Outcome::Stay
            }
            "bt" | "where" | "backtrace" | "w" => {
                self.backtrace(host);
                Outcome::Stay
            }
            "l" | "list" => {
                self.list_command(rest.trim(), host);
                Outcome::Stay
            }
            "trace" => {
                self.trace_command(rest.trim());
                Outcome::Stay
            }
            "catch" => {
                self.catch_command(rest.trim());
                Outcome::Stay
            }
            "source" => self.source_command(rest.trim()),
            other => {
                println!("unknown command {other:?} — try `help`");
                Outcome::Stay
            }
        };
        self.flush_edits(host);
        outcome
    }

    /// `step [n]`, `next [n]` and `finish` differ only in when they stop.
    ///
    /// The count is how many statements to run before stopping again, which is
    /// what makes walking a loop body practical (`next 5`, `step 20`).
    fn step_command(&mut self, which: &str, rest: &str) -> Outcome {
        if which == "finish" && !rest.is_empty() {
            println!("usage: finish");
            return Outcome::Stay;
        }
        let count = match rest {
            "" => 1,
            raw => match raw.parse::<u64>() {
                Ok(n) if n >= 1 => n,
                _ => {
                    println!("usage: {which} [n] (n >= 1)");
                    return Outcome::Stay;
                }
            },
        };
        self.step = match (self.current, self.paused, which) {
            (Some((_, depth)), true, "next") => StepMode::Over(depth, count),
            (Some((_, depth)), true, "finish") => StepMode::Out(depth),
            _ => StepMode::Step(count),
        };
        Outcome::Resume
    }

    fn break_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        match self.resolve_break_target(rest, host) {
            Some(spec) => {
                self.pending = Some(Edit::Add(spec));
                Outcome::Stay
            }
            None => Outcome::Stay,
        }
    }

    /// Resolve a line expression to a line number.
    ///
    /// Forms: `123` (absolute), `+N` / `-N` (relative to the stop), `Func`
    /// (its first statement), and `Func+N` / `Func-N` (entry line plus an
    /// offset). Bounds are the caller's business — `break` rejects an
    /// out-of-range target while `list` clamps.
    fn resolve_line_expr(&self, expr: &str, host: &mut dyn DebugHost) -> Option<u32> {
        let expr = expr.trim();
        if expr.is_empty() {
            return None;
        }
        // `+N` / `-N`: relative to the current stop.
        if expr.starts_with('+') || expr.starts_with('-') {
            let Ok(n) = expr[1..].parse::<i64>() else {
                println!("not a line expression: {expr:?}");
                return None;
            };
            let Some((span, _)) = self.current else {
                println!("no current line to be relative to — run first");
                return None;
            };
            let delta = if expr.starts_with('-') { -n } else { n };
            let line = span.start.line as i64 + delta;
            if line < 1 {
                println!("{expr:?} resolves to line {line}, before the file");
                return None;
            }
            return Some(line as u32);
        }
        if let Ok(line) = expr.parse::<u32>() {
            return Some(line);
        }
        // `Func`, `Func+N`, `Func-N`.
        let (name, offset) = split_function_offset(expr);
        match host.function_entry_line(name) {
            Some(entry) => {
                let line = entry as i64 + offset;
                if line < 1 {
                    println!("{expr:?} resolves to line {line}, before the file");
                    return None;
                }
                Some(line as u32)
            }
            None => {
                println!("no such line or function: {expr:?}");
                None
            }
        }
    }

    /// Parse `break`/`jmp` syntax:
    /// `<line|func> [if <expr>] [skip <n>] [every <n>] [nostop] [do <stmt>]`.
    /// The `do` clause takes the rest of the line; the other options are
    /// stripped from the tail before the `if` condition is split off.
    fn resolve_break_target(&mut self, rest: &str, host: &mut dyn DebugHost) -> Option<BpSpec> {
        let (head, action) = match rest.split_once(" do ") {
            Some((h, a)) => (h.trim(), Some(a.trim().to_string())),
            None => (rest.trim(), None),
        };
        let mut skip = 0u64;
        let mut every = 1u64;
        let mut stop = true;
        let mut head = head.to_string();
        loop {
            let trimmed = head.trim_end();
            if let Some(t) = trimmed.strip_suffix(" nostop") {
                stop = false;
                head = t.to_string();
            } else if let Some(rest) = trimmed.strip_suffix(" nostop") {
                stop = false;
                head = rest.to_string();
            } else if let Some((t, n)) = strip_tail_number(trimmed, "skip") {
                match n {
                    Some(n) => skip = n,
                    None => {
                        println!("usage: ... skip <n>");
                        return None;
                    }
                }
                head = t.to_string();
            } else if let Some((t, n)) = strip_tail_number(trimmed, "every") {
                match n {
                    Some(n) if n >= 1 => every = n,
                    _ => {
                        println!("usage: ... every <n>");
                        return None;
                    }
                }
                head = t.to_string();
            } else {
                break;
            }
        }
        let (pos, condition) = match head.split_once(" if ") {
            Some((p, c)) => (p.trim(), Some(c.trim().to_string())),
            None => match head.split_once("if ") {
                Some((p, c)) => (p.trim(), Some(c.trim().to_string())),
                None => (head.trim(), None),
            },
        };
        let Some(line) = self.resolve_line_expr(pos, host) else {
            return None;
        };
        if line == 0 || line as usize > self.lines.len().max(1) {
            println!(
                "line {line} is outside {} (1..{})",
                self.script,
                self.lines.len()
            );
            return None;
        }
        let mut spec = BpSpec::line(line);
        // A function-relative target still shows its function in `info
        // breakpoints`; plain numbers and `+N`/`-N` have no name.
        if pos.parse::<u32>().is_err() && !pos.starts_with('+') && !pos.starts_with('-') {
            spec.label = Some(format!("func {pos}"));
        }
        spec.condition = condition;
        spec.skip = skip;
        spec.every = every;
        spec.stop = stop;
        if let Some(a) = action {
            spec.actions.push(a);
        }
        Some(spec)
    }

    /// `jmp <line-expr>` — unconditionally transfer execution: statements
    /// between here and the target are skipped without running. Only lines of
    /// the frame that will resume are valid; the interpreter rejects the rest.
    fn jmp_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        if !self.paused {
            println!("jmp needs a stopped run — use run first");
            return Outcome::Stay;
        }
        let Some(line) = self.resolve_line_expr(rest.trim(), host) else {
            println!("usage: jmp <line> (a statement line of the current frame)");
            return Outcome::Stay;
        };
        match host.jump_to(line) {
            Ok(()) => {
                println!("jumping to line {line}");
                Outcome::Resume
            }
            Err(e) => {
                println!("cannot jump: {e}");
                Outcome::Stay
            }
        }
    }

    /// `tbreak <line|func>` — a temporary breakpoint: keep running until the
    /// position is reached or the function is entered; it deletes itself when
    /// it fires. Works before `run` and at a stop.
    fn tbreak_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        if rest.is_empty() {
            println!("usage: tbreak <line|func>");
            return Outcome::Stay;
        }
        match self.resolve_break_target(rest, host) {
            Some(mut spec) => {
                spec.temp = true;
                self.pending = Some(Edit::Add(spec));
                Outcome::Resume
            }
            None => Outcome::Stay,
        }
    }

    /// `untilcall <name>` — run until the next call to that builtin/function.
    ///
    /// `untilcall <func>` — run until the next call to that function and stop
    /// **before** it runs, so the call itself is what is on screen (the same
    /// place `stopat` stops, but one-shot). A builtin like `GUICreate` has no
    /// entry line for `tbreak`, which is why this watches the resolved call
    /// instead; `untilgui` is the shorthand for `untilcall GUICreate`.
    fn until_call_command(&mut self, name: &str) -> Outcome {
        if name.is_empty() {
            println!("usage: untilcall <function>");
            return Outcome::Stay;
        }
        self.until_call = Some(name.to_ascii_lowercase());
        self.until_ret = None;
        self.until_hit = false;
        self.step = StepMode::Run;
        println!("running until {name} is called (stopping before it runs)");
        Outcome::Resume
    }

    /// `untilret <func>` — the other side of `untilcall`: run until the next
    /// call to that function has *returned* and stop on the statement after it.
    /// For a dialog that is after it was answered, and for a script function
    /// after its body ran — what you want when the *result* is the interesting
    /// part.
    fn until_ret_command(&mut self, name: &str) -> Outcome {
        if name.is_empty() {
            println!("usage: untilret <function>");
            return Outcome::Stay;
        }
        self.until_ret = Some(name.to_ascii_lowercase());
        self.until_call = None;
        self.until_hit = false;
        self.step = StepMode::Run;
        println!("running until {name} returns");
        Outcome::Resume
    }

    /// `stopat <func>...` — stop *before* a call to any of these functions,
    /// builtins included, so what the call is about to do can be read first.
    /// The point of it is a dialog's text and a DLL's name: `stopat MsgBox
    /// DllOpen` shows both without either happening, and `continue` then runs
    /// the call. Targets accumulate (`catch syscall <name>` style: one command
    /// per target), `stopat off` clears them all, and `stopat` alone lists
    /// them.
    fn stop_at_command(&mut self, rest: &str) -> Outcome {
        let names: Vec<&str> = rest.split_whitespace().collect();
        if names.is_empty() {
            if self.stop_at.is_empty() {
                println!("no stop-at set (usage: stopat <function>..., e.g. `stopat MsgBox`)")
            } else {
                println!("stopping before every {} call", self.stop_at.join(", "));
            }
            return Outcome::Stay;
        }
        if names.len() == 1 && (names[0].eq_ignore_ascii_case("off") || names[0].eq_ignore_ascii_case("clear")) {
            if self.stop_at.is_empty() {
                println!("no stop-at was set");
            } else {
                println!("stop-at cleared (was {})", self.stop_at.join(", "));
                self.stop_at.clear();
            }
            return Outcome::Stay;
        }
        for name in names {
            let lower = name.to_ascii_lowercase();
            if !self.stop_at.contains(&lower) {
                self.stop_at.push(lower);
            }
        }
        println!("stopping before every {} call", self.stop_at.join(", "));
        Outcome::Stay
    }

    /// `eval <stmt>` — run AutoIt source as a *statement* in the current
    /// frame: assignments take effect on the paused program. `print` is the
    /// expression counterpart.
    fn eval_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        if rest.is_empty() {
            println!("usage: eval <statement>");
            return Outcome::Stay;
        }
        self.show(host.evaluate(rest));
        Outcome::Stay
    }

    /// `ignore <id> <count>` — the next `count` would-be hits do not fire.
    fn ignore_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        let mut parts = rest.split_whitespace();
        let (id, count) = match (parts.next(), parts.next()) {
            (Some(i), Some(c)) => (i, c),
            _ => {
                println!("usage: ignore <id> <count>");
                return Outcome::Stay;
            }
        };
        match (id.parse::<u32>(), count.parse::<u64>()) {
            (Ok(id), Ok(count)) => {
                if host.ignore_breakpoint(id, count) {
                    println!("breakpoint {id} will skip the next {count} hit(s)");
                } else {
                    println!("no breakpoint {id}");
                }
            }
            _ => println!("usage: ignore <id> <count>"),
        }
        Outcome::Stay
    }

    /// `commands <id>` — list on-hit actions; `commands <id> do <stmt>` —
    /// append one; `commands <id> off` — clear them all.
    fn commands_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        let (id_raw, tail) = match rest.split_once(char::is_whitespace) {
            Some((i, t)) => (i, t.trim()),
            None => (rest.trim(), ""),
        };
        let Ok(id) = id_raw.parse::<u32>() else {
            println!("usage: commands <id> [do <stmt> | off]");
            return Outcome::Stay;
        };
        let existing = host
            .breakpoints()
            .into_iter()
            .find(|b| b.id == id)
            .map(|b| b.actions);
        let Some(mut actions) = existing else {
            println!("no breakpoint {id}");
            return Outcome::Stay;
        };
        if tail == "off" {
            host.set_breakpoint_actions(id, Vec::new());
            println!("breakpoint {id}: actions cleared");
        } else if let Some(stmt) = tail.strip_prefix("do ") {
            actions.push(stmt.trim().to_string());
            host.set_breakpoint_actions(id, actions.clone());
            println!("breakpoint {id}: {} action(s)", actions.len());
        } else if tail.is_empty() {
            if actions.is_empty() {
                println!("breakpoint {id}: no actions");
            }
            for a in &actions {
                println!("  do {a}");
            }
        } else {
            println!("usage: commands <id> [do <stmt> | off]");
        }
        Outcome::Stay
    }

    /// `nostop <id>` / `stop <id>` — make a breakpoint a pure logpoint or
    /// restore its stopping behaviour.
    fn stop_toggle_command(
        &mut self,
        rest: &str,
        stop: bool,
        host: &mut dyn DebugHost,
    ) -> Outcome {
        match rest.trim().parse::<u32>() {
            Ok(id) => {
                if host.set_breakpoint_stop(id, stop) {
                    println!(
                        "breakpoint {id} will {}",
                        if stop { "stop" } else { "not stop (logpoint)" }
                    );
                } else {
                    println!("no breakpoint {id}");
                }
            }
            Err(_) => println!("usage: {} <id>", if stop { "stop" } else { "nostop" }),
        }
        Outcome::Stay
    }

    /// `watch` — list; `watch <expr>` — break when the value changes
    /// (first observation only sets the baseline); `watch -d <id>` — remove.
    fn watch_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        let rest = rest.trim();
        if rest.is_empty() {
            if self.watches.is_empty() {
                println!("no watches");
            }
            for w in &self.watches {
                println!(
                    "{:>3}  {}  last={}",
                    w.id,
                    w.expr,
                    w.last.as_deref().unwrap_or("(not observed yet)")
                );
            }
            return Outcome::Stay;
        }
        if let Some(id) = rest.strip_prefix("-d ") {
            match id.trim().parse::<u32>() {
                Ok(id) => {
                    let before = self.watches.len();
                    self.watches.retain(|w| w.id != id);
                    println!(
                        "{}",
                        if self.watches.len() != before {
                            format!("deleted watch {id}")
                        } else {
                            format!("no watch {id}")
                        }
                    );
                }
                Err(_) => println!("usage: watch -d <id>"),
            }
            return Outcome::Stay;
        }
        if self.paused {
            // A baseline taken at the current stop: changes after the resume
            // are what stop.
            match host.evaluate_expression(rest) {
                Ok(v) => {
                    let id = self.next_watch;
                    self.next_watch += 1;
                    println!("watch {id}: {rest} = {}", format_value(&v));
                    self.watches.push(Watch { id, expr: rest.to_string(), last: Some(format!("{v:?}")) });
                }
                Err(e) => println!("cannot evaluate {rest:?} here: {e}"),
            }
        } else {
            let id = self.next_watch;
            self.next_watch += 1;
            println!("watch {id}: {rest} (baseline set on first write)");
            self.watches.push(Watch { id, expr: rest.to_string(), last: None });
        }
        Outcome::Stay
    }

    fn unwatch_command(&mut self, rest: &str) -> Outcome {
        match rest.trim().parse::<u32>() {
            Ok(id) => {
                let before = self.watches.len();
                self.watches.retain(|w| w.id != id);
                println!(
                    "{}",
                    if self.watches.len() != before {
                        format!("deleted watch {id}")
                    } else {
                        format!("no watch {id}")
                    }
                );
            }
            Err(_) => println!("usage: unwatch <id>"),
        }
        Outcome::Stay
    }

    fn delete_command(&mut self, rest: &str) -> Outcome {
        let target = if rest.trim().is_empty() {
            None
        } else {
            match rest.trim().parse::<u32>() {
                Ok(id) => Some(id),
                Err(_) => {
                    println!("usage: delete [id]");
                    return Outcome::Stay;
                }
            }
        };
        self.pending = Some(Edit::Remove(target));
        Outcome::Stay
    }

    fn toggle_command(&mut self, rest: &str, enabled: bool) -> Outcome {
        match rest.trim().parse::<u32>() {
            Ok(id) => {
                self.pending = Some(Edit::Enable(id, enabled));
                Outcome::Stay
            }
            Err(_) => {
                println!("usage: {} <id>", if enabled { "enable" } else { "disable" });
                Outcome::Stay
            }
        }
    }

    /// Apply the breakpoint edit the command recorded.
    fn flush_edits(&mut self, host: &mut dyn DebugHost) {
        match self.pending.take() {
            None => {}
            Some(Edit::Add(spec)) => {
                let id = host.add_breakpoint_full(
                    spec.line,
                    spec.condition.clone(),
                    spec.skip,
                    spec.every,
                    spec.stop,
                    spec.actions.clone(),
                );
                if let Some(label) = &spec.label {
                    self.bp_labels.push((id, label.clone()));
                }
                if spec.temp {
                    self.jmp_pending.push(id);
                }
                let mut summary = if let Some(label) = &spec.label {
                    format!("Breakpoint {id} at {label} (line {})", spec.line)
                } else {
                    format!("Breakpoint {id} at line {}", spec.line)
                };
                if let Some(c) = &spec.condition {
                    summary.push_str(&format!(" if {c}"));
                }
                if spec.skip > 0 {
                    summary.push_str(&format!(", skips next {}", spec.skip));
                }
                if spec.every > 1 {
                    summary.push_str(&format!(", every {}", spec.every));
                }
                if !spec.stop {
                    summary.push_str(", nostop (logpoint)");
                }
                if !spec.actions.is_empty() {
                    summary.push_str(&format!(", do {:?}", spec.actions));
                }
                println!("{summary}");
            }
            Some(Edit::Remove(Some(id))) => {
                if host.remove_breakpoint(id) {
                    println!("deleted breakpoint {id}");
                } else {
                    println!("no breakpoint {id}");
                }
            }
            Some(Edit::Remove(None)) => {
                let ids: Vec<u32> = host.breakpoints().iter().map(|b| b.id).collect();
                for id in &ids {
                    host.remove_breakpoint(*id);
                }
                println!("deleted {} breakpoints", ids.len());
            }
            Some(Edit::Enable(id, enabled)) => {
                if host.set_breakpoint_enabled(id, enabled) {
                    println!(
                        "breakpoint {id} {}",
                        if enabled { "enabled" } else { "disabled" }
                    );
                } else {
                    println!("no breakpoint {id}");
                }
            }
        }
        // Keep the specs in step with the host, so a restart can restore them.
        // Unfired one-shot (`tbreak`) breakpoints are carried across a
        // restart; fired ones self-deleted and are already gone.
        self.saved_breakpoints = host
            .breakpoints()
            .iter()
            .map(|b| {
                (
                    b.clone(),
                    self.bp_labels
                        .iter()
                        .find(|(id, _)| *id == b.id)
                        .map(|(_, l)| l.clone()),
                )
            })
            .collect();
    }

    fn eval_and_print(&mut self, expr: &str, host: &mut dyn DebugHost) {
        if expr.is_empty() {
            println!("usage: print <expression>");
            return;
        }
        // An expression, so `p $i = 5` compares rather than assigns — `set` is
        // the command that assigns, and it says so. With a frame selected the
        // evaluation happens there, the way gdb's `print` works.
        let value = match self.frame_depth(host.frames().len()) {
            Some(depth) => host.evaluate_expression_in_frame(depth, expr),
            None => host.evaluate_expression(expr),
        };
        self.show(value);
    }

    fn set_command(&mut self, rest: &str, host: &mut dyn DebugHost) {
        if rest.is_empty() {
            println!("usage: set $var = <expression>");
            return;
        }
        // Accept both `set $x = 1` and `set $x 1`, like gdb does.
        let assignment = match rest.split_once('=') {
            Some((lhs, rhs)) => format!("{} = {}", lhs.trim(), rhs.trim()),
            None => rest.to_string(),
        };
        self.show(host.evaluate(&assignment));
    }

    fn show(&mut self, result: Result<autoitv3_runtime::Value, autoitv3_runtime::RuntimeError>) {
        match result {
            Ok(v) => println!("{}", format_value(&v)),
            Err(e) => println!("{e}"),
        }
    }

    fn info_command(&mut self, rest: &str, host: &mut dyn DebugHost) {
        match rest.trim() {
            "b" | "break" | "breakpoints" => self.info_breakpoints(host),
            "lo" | "local" | "locals" | "variables" | "v" => self.info_locals(host),
            "g" | "global" | "globals" => self.info_globals(host),
            "f" | "func" | "funcs" | "functions" => self.info_functions(host),
            "frame" | "stack" => self.backtrace(host),
            "" => println!("usage: info breakpoints|locals|globals|functions|frame"),
            other => println!("unknown info topic {other:?}"),
        }
    }

    fn info_breakpoints(&mut self, host: &mut dyn DebugHost) {
        let bps = host.breakpoints();
        if bps.is_empty() {
            println!("no breakpoints");
            return;
        }
        for bp in bps {
            let state = if bp.enabled { "y" } else { "n" };
            let label = self
                .bp_labels
                .iter()
                .find(|(id, _)| *id == bp.id)
                .map(|(_, l)| l.as_str())
                .unwrap_or("line");
            let condition = match &bp.condition {
                Some(c) => format!(" if {c}"),
                None => String::new(),
            };
            println!(
                "{:>3}  {} {:<6} enabled={state}  hits={}{}  skip={} every={}{}{}",
                bp.id,
                label,
                bp.line,
                bp.hits,
                condition,
                bp.skip_remaining,
                bp.every,
                if bp.stop { "" } else { "  nostop" },
                if bp.actions.is_empty() {
                    String::new()
                } else {
                    format!("  actions={}", bp.actions.len())
                },
            );
            for a in &bp.actions {
                println!("        do {a}");
            }
        }
    }

    fn info_locals(&mut self, host: &mut dyn DebugHost) {
        if !self.paused {
            println!("the script is not stopped — use `run` first");
            return;
        }
        let frames = host.frames();
        let index = self.frame_index(frames.len());
        let Some(frame) = frames.get(index) else {
            // Top-level code runs without a frame; its variables are globals.
            println!("the top level has no locals — see `info globals`");
            return;
        };
        if frame.locals.is_empty() {
            let name = frame.function.as_deref().unwrap_or("<script>");
            println!("no locals in {name}");
            return;
        }
        for (name, value) in &frame.locals {
            println!("{name} = {}", format_value(value));
        }
    }

    fn info_globals(&mut self, host: &mut dyn DebugHost) {
        let globals = host.globals();
        println!("{} globals", globals.len());
        for (name, value) in globals.iter().take(200) {
            println!("{name} = {}", format_value(value));
        }
        if globals.len() > 200 {
            println!("... {} more", globals.len() - 200);
        }
    }

    fn info_functions(&mut self, host: &mut dyn DebugHost) {
        let names = host.function_names();
        println!("{} functions", names.len());
        for chunk in names.chunks(4) {
            println!("  {}", chunk.join("  "));
        }
    }

    fn backtrace(&mut self, host: &mut dyn DebugHost) {
        if !self.paused {
            println!("the script is not stopped — use `run` first");
            return;
        }
        let frames = host.frames();
        let file = self.script_file();
        if frames.is_empty() {
            // Top-level code: there is no activation record to show, but the
            // position still is one.
            match self.current {
                Some((span, _)) => println!("#0  <script> at {file}:{}", span.start.line),
                None => println!("#0  <script>"),
            }
            return;
        }
        // Innermost first, numbered from zero, the way gdb prints a stack. The
        // selected frame (after `frame`/`up`/`down`) is tagged, which gdb leaves
        // to the `frame` command and pdb marks with `>`.
        let selected = self.selected_frame.unwrap_or(0);
        let file = self.script_file();
        for (i, frame) in frames.iter().rev().enumerate() {
            let tag = if i == selected && selected != 0 {
                "  (selected)"
            } else {
                ""
            };
            println!("{}{tag}", self.frame_line(i, frame, &file));
        }
    }

    /// One `bt`/`frame` line: `#0  Name(a=1, b="x") at file:line`.
    fn frame_line(&self, number: usize, frame: &FrameInfo, file: &str) -> String {
        let name = frame.function.as_deref().unwrap_or("<script>");
        let args: Vec<String> = frame
            .params
            .iter()
            .map(|(k, v)| format!("{k}={}", format_value(v)))
            .collect();
        // gdb always prints the parentheses, empty ones included.
        let args = format!("({})", args.join(", "));
        let at = frame
            .span
            .map(|s| format!("{file}:{}", s.start.line))
            .unwrap_or_else(|| "-".to_string());
        format!("#{number}  {name}{args} at {at}")
    }

    /// The file name `bt` shows for every frame (one script, so one file).
    fn script_file(&self) -> String {
        Path::new(&self.script)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.script.clone())
    }

    /// The frame the commands should act on, as an index into `host.frames()`
    /// (which is outermost first). The innermost frame is the default.
    fn frame_index(&self, count: usize) -> usize {
        match self.selected_frame {
            Some(number) => count.saturating_sub(1 + number),
            None => count.saturating_sub(1),
        }
    }

    /// The same index as the `depth` [`FrameInfo::depth`] reports, for the host
    /// calls that evaluate in a frame.
    fn frame_depth(&self, count: usize) -> Option<usize> {
        match self.selected_frame {
            Some(number) if count > 1 => Some(count.saturating_sub(1 + number)),
            _ => None,
        }
    }

    /// `frame [number]` — select a frame (what gdb's `frame` does). With no
    /// number it reports the selected one.
    fn frame_command(&mut self, rest: &str, host: &mut dyn DebugHost) {
        if !self.paused {
            println!("the script is not stopped — use `run` first");
            return;
        }
        let count = host.frames().len();
        if count == 0 {
            println!("only top-level code is on the stack (`#0  <script>`)");
            return;
        }
        if !rest.is_empty() {
            let number = rest.split_whitespace().next().unwrap_or("").parse::<usize>();
            match number {
                Ok(n) if n < count => self.selected_frame = (n != 0).then_some(n),
                Ok(n) => {
                    println!("no frame {n}: the stack has {count} (0..{})", count - 1);
                    return;
                }
                Err(_) => {
                    println!("usage: frame [number]");
                    return;
                }
            }
        }
        self.show_frame(host);
    }

    /// `up [n]` / `down [n]` — move the selection: `up` goes toward the caller
    /// (gdb numbers grow outward), `down` toward the innermost frame.
    fn move_frame_command(&mut self, word: &str, rest: &str, host: &mut dyn DebugHost) {
        if !self.paused {
            println!("the script is not stopped — use `run` first");
            return;
        }
        let count = host.frames().len();
        if count == 0 {
            println!("only top-level code is on the stack");
            return;
        }
        let step = rest
            .split_whitespace()
            .next()
            .map(|w| w.parse::<usize>().unwrap_or(1))
            .unwrap_or(1)
            .max(1);
        let current = self.selected_frame.unwrap_or(0);
        let next = if word.eq_ignore_ascii_case("up") {
            (current + step).min(count - 1)
        } else {
            current.saturating_sub(step)
        };
        self.selected_frame = (next != 0).then_some(next);
        self.show_frame(host);
    }

    /// Report the selected frame the way gdb's `frame` does: the line, then the
    /// source at that frame's position.
    fn show_frame(&mut self, host: &mut dyn DebugHost) {
        let frames = host.frames();
        let index = self.frame_index(frames.len());
        let Some(frame) = frames.get(index) else {
            println!("no frames — only top-level code is on the stack");
            return;
        };
        let number = frames.len() - 1 - index;
        let file = self.script_file();
        println!("{}", self.frame_line(number, frame, &file));
        if let Some(span) = frame.span {
            if let Some(line) = self.lines.get(span.start.line.saturating_sub(1) as usize) {
                println!("{}  {}", span.start.line, line.trim());
            }
        }
    }

    /// `list [line-expr]`, showing a window around the stop point.
    ///
    /// Beyond the absolute form this accepts the same expressions as the
    /// breakpoint commands (`+1`, `-1`, `Main`, `Main-1`); a bare `+` keeps its
    /// older "one line past the stop" meaning.
    fn list_command(&mut self, rest: &str, host: &mut dyn DebugHost) {
        if self.lines.is_empty() {
            println!("{} is empty", self.script);
            return;
        }
        let current = self.current.map(|(span, _)| span.start.line);
        let centre = match rest.trim() {
            "" => current.unwrap_or(1),
            "+" => current.map(|l| l.saturating_add(1)).unwrap_or(1),
            other => match self.resolve_line_expr(other, host) {
                Some(line) => line,
                None => return,
            },
        };
        // Clamp into the file, so `list 999999` shows the tail rather than
        // nothing at all.
        let centre = centre.clamp(1, self.lines.len() as u32);
        let first = centre.saturating_sub(4).max(1);
        let last = (first + 8).min(self.lines.len() as u32);
        for line in first..=last {
            let marker = if Some(line) == current { "=>" } else { "  " };
            println!("{marker} {line:>6}  {}", self.lines[(line - 1) as usize]);
        }
    }

    fn catch_command(&mut self, rest: &str) {
        match rest {
            "on" => {
                self.catching = true;
                println!("stopping where an uncaught error is raised");
            }
            "off" => {
                self.catching = false;
                println!("uncaught errors will end the run without stopping");
            }
            "" => println!(
                "stopping on uncaught errors is {}",
                if self.catching { "on" } else { "off" }
            ),
            other => println!("usage: catch on|off (not {other:?})"),
        }
    }

    /// `source FILE` — queue that file's commands to run next.
    fn source_command(&mut self, path: &str) -> Outcome {
        if path.is_empty() {
            println!("usage: source <file>");
            return Outcome::Stay;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let commands: Vec<String> = text.lines().flat_map(split_commands).collect();
                let count = commands.len();
                // Pushed to the front, so they run before anything already
                // queued — including from inside a breakpoint's prompt.
                for command in commands.into_iter().rev() {
                    self.queue.push_front(command);
                }
                println!("sourced {path} ({count} commands)");
                Outcome::Stay
            }
            Err(e) => {
                println!("cannot read {path}: {e}");
                Outcome::Stay
            }
        }
    }

    /// `trace on|off`, plus filters that keep a hot loop from flooding the
    /// output: `trace depth <n>` (only depth <= n) and `trace skip <func>`
    /// (suppress while that function is on the stack).
    fn trace_command(&mut self, rest: &str) {
        let (word, arg) = match rest.split_once(char::is_whitespace) {
            Some((w, a)) => (w, a.trim()),
            None => (rest, ""),
        };
        match word {
            "on" => {
                self.tracing = true;
                println!("statement tracing on");
            }
            "off" => {
                self.tracing = false;
                println!("statement tracing off");
            }
            "depth" => match arg {
                "off" => {
                    self.trace_depth = None;
                    println!("trace depth limit off");
                }
                other => match other.parse::<usize>() {
                    Ok(n) => {
                        self.trace_depth = Some(n);
                        println!("tracing statements at depth <= {n}");
                    }
                    Err(_) => println!("usage: trace depth <n> | trace depth off"),
                },
            },
            "skip" => match arg {
                "off" => {
                    self.trace_skip.clear();
                    println!("trace skip list cleared");
                }
                "" => println!("usage: trace skip <func> | trace skip off"),
                other => {
                    let lower = other.to_ascii_lowercase();
                    if !self.trace_skip.contains(&lower) {
                        self.trace_skip.push(lower);
                    }
                    println!("not tracing inside {other}");
                }
            },
            "unskip" => {
                let lower = arg.to_ascii_lowercase();
                self.trace_skip.retain(|f| *f != lower);
                println!("tracing inside {arg} again");
            }
            "" => {
                let mut state = if self.tracing { "on" } else { "off" }.to_string();
                if let Some(n) = self.trace_depth {
                    state.push_str(&format!(", depth <= {n}"));
                }
                if !self.trace_skip.is_empty() {
                    state.push_str(&format!(", skipping {}", self.trace_skip.join(", ")));
                }
                println!("statement tracing is {state}");
            }
            other => println!(
                "usage: trace on|off | trace depth <n> | trace skip <func> (not {other:?})"
            ),
        }
    }

    /// Whether `trace` echoes a statement at `depth`.
    fn trace_visible(&self, depth: usize) -> bool {
        if self.trace_depth.is_some_and(|max| depth > max) {
            return false;
        }
        !self
            .call_stack
            .iter()
            .any(|frame| self.trace_skip.iter().any(|skip| skip == frame))
    }

    fn print_help(&mut self, topic: &str) {
        if !topic.is_empty() {
            println!("{}", help_for(topic));
            return;
        }
        println!(
            "\
Commands (`help <cmd>` describes one)

  run, restart           start the script (a second `run` starts over)
  continue, c            resume until the next breakpoint
  step [n], s            run n statements (default 1), entering calls
  next [n], n            run n statements in this frame (default 1)
  finish, fin            run until the current function returns
  until <line-expr>      alias of `tbreak <line-expr>`
  untilcall <func>, uc   run until <func> is called, stopping *before* it runs
  untilret <func>, ur    run until <func> has returned (stop after the call)
  untilgui, gui          untilcall GUICreate
  stopat <func>..., sa   stop *before* any of these are called — a builtin
                         before it runs (`stopat MsgBox DllOpen` shows a
                         dialog's text and a DLL's name without either
                         happening), a script function at its entry
  stopat                 list the targets; `stopat off` clears them all
  break <line-expr> [if E]   set a breakpoint, optionally conditional
  jmp <line-expr>        skip statements up to the target line
  delete [id]            remove one breakpoint, or all of them
  enable/disable <id>    toggle a breakpoint
  print <expr>, p        evaluate an expression in the current frame
  set $x = <expr>        assign to a variable in the current frame
  info <topic>           breakpoints | locals | globals | functions | frame
  backtrace, bt          show the call stack (innermost first, like gdb)
  frame [n], f           select a frame: what `print`/`info locals`/`list` see
  up [n], down [n]       move the selection outward / inward
  list [line-expr], l    show source around the stop point
  trace on|off           echo every statement (filters: depth N, skip Func)
  catch on|off           stop where an uncaught error is raised (default on)
  source <file>          run the commands in a file, then come back here
  quit, q                leave the session

A <line-expr> is `123`, `+N`/`-N` (relative to the stop), `Func`, or
`Func+N`/`Func-N` (the function's entry line plus an offset).

`;` separates several commands on one line (`break 11; run; print $i`); a `;`
inside a \"quoted string\" is left alone."
        );
    }
}

impl Debugger for Shell {
    fn on_error(&mut self, error: &RuntimeError, span: Option<Span>, host: &mut dyn DebugHost) {
        if !self.catching || self.finished {
            return;
        }
        self.reported_error = true;
        self.paused = true;
        // The statement that raised the error is the one the frame is on.
        if let Some(span) = span {
            let depth = self.current.map(|(_, d)| d).unwrap_or(0);
            self.current = Some((span, depth));
        }
        println!("[uncaught error] {error}");
        self.show_current_line();
        // The frames that led here are still live, so `backtrace` and
        // `info locals` work and `run` starts a fresh pass. Whatever is typed,
        // the error carries on unwinding when this returns.
        self.prompt_loop(host);
        self.paused = false;
    }

    fn on_breakpoint_action(&mut self, bp: &Breakpoint, host: &mut dyn DebugHost) {
        // The actions are debugger commands (`print`, `eval`, `set`, `jmp`,
        // ...), run through the ordinary command parser with the live host —
        // a logpoint is just a `nostop` breakpoint whose commands print.
        for cmd in &bp.actions {
            let outcome = self.execute(cmd, host);
            if matches!(outcome, Outcome::Quit) {
                self.finished = true;
            }
        }
    }

    fn on_variable_write(&mut self, _name: &str, _value: &autoitv3_runtime::Value) {
        // No host here: the actual re-observation happens at the next
        // statement boundary, where `on_statement` has one.
        if !self.paused && !self.finished && !self.watches.is_empty() {
            self.pending_watch_check = true;
        }
    }

    fn on_call_enter(
        &mut self,
        name: &str,
        args: &[autoitv3_runtime::Value],
    ) -> DebugAction {
        self.call_stack.push(name.to_ascii_lowercase());
        self.catch_action(name, args)
    }

    fn on_call_exit(&mut self, name: &str, _result: Option<&autoitv3_runtime::Value>) {
        self.call_stack.pop();
        // A script function's "after the call" is the caller's next statement,
        // so the `untilret` flag goes up as the frame unwinds — not at entry,
        // which would stop on the body's first statement.
        if self.until_ret_matches(name) {
            self.until_hit = true;
        }
    }

    fn on_builtin_call(&mut self, name: &str, args: &[autoitv3_runtime::Value]) -> DebugAction {
        // A builtin has no entry line, so `untilcall GUICreate` watches the
        // resolved call instead (`GUICreate` still goes through the table and
        // reaches here as a function value). It also has no exit hook, so for
        // `untilret` *this* is the moment before the statement that follows the
        // call.
        if self.until_ret_matches(name) {
            self.until_hit = true;
        }
        self.catch_action(name, args)
    }

    fn on_statement(&mut self, span: Span, depth: usize, host: &mut dyn DebugHost) -> DebugAction {
        // Unwind the run when the session is over, or when `run` asked for a
        // fresh one from inside a stop.
        if self.finished || self.restart {
            return DebugAction::Abort;
        }
        // A write happened since the last statement: re-observe the watches.
        let watch_fired = if self.pending_watch_check {
            self.pending_watch_check = false;
            self.check_watches(host)
        } else {
            false
        };
        if self.tracing && self.trace_visible(depth) {
            println!("[trace] {}:{} depth={depth}", span.start.line, span.start.col);
        }
        // `untilret` saw its target return: stop here, on the statement after
        // the call.
        let until = std::mem::take(&mut self.until_hit);
        if until {
            self.until_ret = None;
        }
        // `step n`/`next n` count down as statements go by; the mode is cleared
        // on the stop, so the prompt starts from a clean `Run`.
        let stop = until
            || match &mut self.step {
                StepMode::Run => false,
                StepMode::Step(n) => {
                    *n = n.saturating_sub(1);
                    *n == 0
                }
                StepMode::Over(d, n) => {
                    if depth <= *d {
                        *n = n.saturating_sub(1);
                    }
                    *n == 0 && depth <= *d
                }
                StepMode::Out(d) => depth < *d,
            };
        self.current = Some((span, depth));
        if stop || watch_fired {
            // Clear the policy: the prompt decides what happens next.
            self.step = StepMode::Run;
            DebugAction::Pause
        } else {
            DebugAction::Continue
        }
    }

    fn on_stop(&mut self, reason: &StopReason, host: &mut dyn DebugHost) {
        match reason {
            // End-of-run notifications. The driver reports the outcome, so
            // there is nothing to prompt for and nothing to print here.
            StopReason::Finished | StopReason::Error => {
                self.paused = false;
            }
            StopReason::Breakpoint { id, line } => {
                self.paused = true;
                self.selected_frame = None;
                if self.jmp_pending.iter().any(|t| t == id) {
                    // A `jmp` target: one-shot, so remove it on arrival.
                    self.jmp_pending.retain(|t| t != id);
                    host.remove_breakpoint(*id);
                    println!("run-to target reached, line {line}");
                } else {
                    println!("Breakpoint {id}, line {line}");
                }
                self.show_current_line();
                self.prompt_loop(host);
                self.paused = false;
            }
            StopReason::Call { name } => {
                self.paused = true;
                self.selected_frame = None;
                match self.caught_call.take() {
                    Some(call) => println!("Catchpoint: {call}"),
                    None => println!("Catchpoint: {name}"),
                }
                self.show_current_line();
                self.prompt_loop(host);
                self.paused = false;
            }
            StopReason::Step | StopReason::Pause => {
                self.paused = true;
                self.selected_frame = None;
                match self.current {
                    Some((span, _)) => println!("Stopped at line {}", span.start.line),
                    None => println!("Stopped"),
                }
                self.show_current_line();
                self.prompt_loop(host);
                self.paused = false;
            }
        }
    }
}

impl Shell {
    /// Whether `untilret` is waiting for this function.
    fn until_ret_matches(&self, name: &str) -> bool {
        self.until_ret
            .as_deref()
            .is_some_and(|target| target.eq_ignore_ascii_case(name))
    }

    /// What the call-entry hooks do: `untilcall`/`stopat` stop *before* the
    /// call, reporting it and its arguments. `untilret` is handled by the
    /// caller of this — at the builtin hook for a builtin, at the exit hook for
    /// a script function. Stops asked for from inside a stop are ignored —
    /// `print` runs calls too.
    fn catch_action(&mut self, name: &str, args: &[autoitv3_runtime::Value]) -> DebugAction {
        if !self.paused {
            let one_shot = self
                .until_call
                .as_deref()
                .is_some_and(|target| target.eq_ignore_ascii_case(name));
            let persistent = self
                .stop_at
                .iter()
                .any(|target| target.eq_ignore_ascii_case(name));
            if one_shot || persistent {
                if one_shot {
                    self.until_call = None;
                }
                self.caught_call = Some(format_call(name, args));
                return DebugAction::Pause;
            }
        }
        DebugAction::Continue
    }

    /// Re-observe every watch expression in the current frame; stop when one
    /// changed from its last observed value. The first observation after
    /// `watch` (or after a restart) only sets the baseline.
    fn check_watches(&mut self, host: &mut dyn DebugHost) -> bool {
        let mut fired = false;
        for w in &mut self.watches {
            let Ok(v) = host.evaluate_expression(&w.expr) else {
                continue;
            };
            let now = format!("{v:?}");
            if w.last.is_none() {
                // First observation: baseline only.
                w.last = Some(now);
                continue;
            }
            if w.last.as_deref() != Some(now.as_str()) {
                let old = w.last.clone().unwrap_or_default();
                println!(
                    "[watch {id}] {expr}: {old} -> {new}",
                    id = w.id,
                    expr = w.expr,
                    new = now
                );
                w.last = Some(now);
                fired = true;
            }
        }
        fired
    }

    /// Print the source line execution is stopped on.
    fn show_current_line(&mut self) {
        let Some((span, _)) = self.current else { return };
        let line = span.start.line;
        if let Some(text) = self.lines.get(line.saturating_sub(1) as usize) {
            println!("{line:>6}  {text}");
        }
    }

    /// Read commands until one resumes execution.
    fn prompt_loop(&mut self, host: &mut dyn DebugHost) {
        loop {
            let Some(cmd) = self.next_logical(Some(host)) else {
                // EOF at a prompt means "stop", the same as gdb.
                self.finished = true;
                return;
            };
            match self.execute(&cmd, host) {
                Outcome::Stay => {}
                Outcome::Resume | Outcome::Quit => return,
            }
        }
    }

    /// Read one *logical* command.
    ///
    /// Two commands take a **multi-line block**: bare `eval` (AutoIt source
    /// until a line reading `end`), and bare `commands <id>` (debugger
    /// commands until `end`, applied as the breakpoint's on-hit actions).
    /// Everything else — including an `eval` whose argument already carries
    /// embedded newlines — passes through as one line.
    fn next_logical(&mut self, mut host: Option<&mut dyn DebugHost>) -> Option<String> {
        loop {
        let cmd = self.next_command(host.as_deref())?;
        let trimmed = cmd.trim_start();
        let (first, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((w, r)) => (w, r.trim_start()),
            None => (trimmed, ""),
        };
        let word = first.to_ascii_lowercase();

        // Bare `eval`: collect AutoIt source until `end`.
        if word == "eval" && rest.is_empty() {
            let mut body = String::new();
            loop {
                match self.next_command(host.as_deref()) {
                    None => break, // EOF: finalize with what we have
                    Some(l) => {
                        if l.trim().eq_ignore_ascii_case("end") {
                            break;
                        }
                        body.push_str(&l);
                        body.push('\n');
                    }
                }
            }
            return Some(format!("eval {body}"));
        }

        // Bare `commands <id>`: collect debugger commands until `end`.
        if word == "commands" {
            if let Ok(id) = rest.parse::<u32>() {
                let mut actions: Vec<String> = Vec::new();
                loop {
                    match self.next_command(host.as_deref()) {
                        None => break,
                        Some(l) => {
                            if l.trim().eq_ignore_ascii_case("end") {
                                break;
                            }
                            if !l.trim().is_empty() {
                                actions.push(l.trim().to_string());
                            }
                        }
                    }
                }
                match host.as_deref_mut() {
                    Some(h) => {
                        h.set_breakpoint_actions(id, actions.clone());
                        println!("breakpoint {id}: {} action(s)", actions.len());
                    }
                    None => println!("no host to apply breakpoint actions"),
                }
                continue; // block handled: read the next command
            }
            return Some(cmd); // `commands <id> do …` / `off` stay one-liners
        }

        // An `eval` written as one -c argument with embedded newlines: strip a
        // trailing lone `end` line, so blocks read the same everywhere.
        if word == "eval" && rest.contains('\n') {
            if let Some(pos) = rest.rfind('\n') {
                if rest[pos + 1..].trim().eq_ignore_ascii_case("end") {
                    let body = &rest[..pos + 1];
                    return Some(format!("eval {body}"));
                }
            }
        }
        return Some(cmd);
        }
    }
}

/// Split a command line into the commands it holds.
///
/// `;` separates commands so `-c "break 11; run"` and one line of a command
/// file can carry several. A `;` inside a double-quoted string is left alone,
/// and so is its AutoIt idiom for a literal quote (`""`), which is why the
/// in-string flag is a toggle rather than a scan for the next quote.
fn split_commands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for c in line.chars() {
        match c {
            '"' => {
                in_string = !in_string;
                current.push(c);
            }
            ';' if !in_string => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty() && !c.starts_with('#'))
        .collect()
}

/// Strip a trailing `skip <n>`-style option off a break spec, returning the
/// head and the parsed number.
fn strip_tail_number<'a>(head: &'a str, keyword: &str) -> Option<(&'a str, Option<u64>)> {
    let idx = head.rfind(keyword)?;
    let tail = head[idx + keyword.len()..].trim();
    let n = tail.parse::<u64>().ok()?;
    Some((head[..idx].trim_end(), Some(n)))
}

/// Split a `func+N` / `func-N` line expression into its name and offset; a bare
/// name has offset 0.
///
/// AutoIt identifiers cannot contain `+`/`-`, so the last such operator is
/// always the offset separator.
fn split_function_offset(expr: &str) -> (&str, i64) {
    if let Some((name, off)) = expr.rsplit_once('+') {
        if let Ok(n) = off.parse::<i64>() {
            return (name, n);
        }
    }
    if let Some((name, off)) = expr.rsplit_once('-') {
        if let Ok(n) = off.parse::<i64>() {
            return (name, -n);
        }
    }
    (expr, 0)
}

/// Split `"break 12"` into `("break", "12")`.
fn split_command(line: &str) -> (String, String) {
    match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word.to_ascii_lowercase(), rest.to_string()),
        None => (line.to_ascii_lowercase(), String::new()),
    }
}

/// A call as the catchpoint banner shows it: the spelling the script used and
/// the arguments it is about to be given, quoted the way AutoIt source is.
fn format_call(name: &str, args: &[autoitv3_runtime::Value]) -> String {
    if args.is_empty() {
        return format!("{name}()");
    }
    let rendered: Vec<String> = args.iter().map(format_value).collect();
    format!("{name}({})", rendered.join(", "))
}

/// One-line help for a single command.
fn help_for(topic: &str) -> String {
    match topic {
        "run" | "r" => "run — start the script body; typing it again starts over".to_string(),
        "continue" | "c" => "continue — resume until the next breakpoint".to_string(),
        "step" | "s" => {
            "step [n] — run n statements (default 1), entering function calls".to_string()
        }
        "next" | "n" => {
            "next [n] — run n statements in this frame or a shallower one".to_string()
        }
        "finish" => "finish — run until the current function returns".to_string(),
        "until" | "u" => "until <line-expr> — alias of `tbreak <line-expr>`".to_string(),
        "untilcall" | "untilc" | "uc" => {
            "untilcall <func> — run until <func> is called, stopping before it runs (builtins too); one-shot".to_string()
        }
        "untilret" | "untilr" | "ur" => {
            "untilret <func> — run until <func> has returned, stopping on the statement after the call".to_string()
        }
        "untilgui" | "gui" => "untilgui — run until GUICreate is called".to_string(),
        "stopat" | "sa" => {
            "stopat <func>... — stop before any of these are called: a builtin before it \
             runs, a script function at its entry (parameters bound). `stopat MsgBox \
             DllOpen` reads a dialog's text and a DLL's name without either happening; \
             `stopat` lists them, `stopat off` clears them"
                .to_string()
        }
        "break" | "b" => {
            "break <line-expr> [if <expr>] [skip <n>] [every <n>] [nostop] [do <cmd>] — stop there; do runs debugger commands on hit".to_string()
        }
        "commands" => {
            "commands <id> [do <cmd> | off] — on-hit debugger commands (run even with nostop)".to_string()
        }
        "jmp" | "j" => {
            "jmp <line-expr> — unconditionally jump: skip statements up to the target line".to_string()
        }
        "tbreak" | "tb" => {
            "tbreak <line-expr> — one-shot breakpoint: run until it is reached".to_string()
        }
        "eval" => {
            "eval <stmt> — run AutoIt source as a statement (assignments stick)".to_string()
        }
        "ignore" => {
            "ignore <id> <count> — the next <count> would-be hits do not fire".to_string()
        }
        "nostop" | "stop" => {
            "nostop <id> | stop <id> — logpoint mode on/off".to_string()
        }
        "watch" => {
            "watch [expr | -d <id>] — break when the expression's value changes".to_string()
        }
        "unwatch" => {
            "unwatch <id> — remove a watch".to_string()
        }
        "print" | "p" => {
            "print <expr> — evaluate in the stopped frame (`p $x`, `p $a[2]`)".to_string()
        }
        "set" => "set $var = <expr> — assign in the stopped frame".to_string(),
        "info" | "i" => "info breakpoints|locals|globals|functions|frame".to_string(),
        "backtrace" | "bt" | "where" => "backtrace — the call stack, innermost first (gdb numbering: #0 is the innermost)".to_string(),
        "frame" | "f" => {
            "frame [n] — select a frame (gdb numbering: #0 innermost); print/info locals act there".to_string()
        }
        "up" | "down" => "up [n] / down [n] — move the frame selection toward the caller / innermost".to_string(),
        "list" | "l" => "list [line-expr] — eight source lines around the stop point".to_string(),
        "trace" => {
            "trace on|off | trace depth <n> | trace skip <func> — echo statements, optionally filtered".to_string()
        }
        "catch" => {
            "catch on|off — stop where an uncaught error is raised, before it unwinds. \
             gdb's equivalent is `catch throw`; its target-taking catchpoints \
             (`catch syscall <name>`, `catch load <lib>`) are what `stopat` is like"
                .to_string()
        }
        "source" => "source <file> — queue the commands in a file, one per line".to_string(),
        "quit" | "q" => "quit — leave the session".to_string(),
        other => format!("no help for {other:?}"),
    }
}

// Unit tests live under `tests/unit/`; `#[path]` pulls the file back in as a
// module of this crate so it can reach the private completer and helpers.
#[cfg(test)]
#[path = "../../tests/unit/debug_completion.rs"]
mod completion_tests;
