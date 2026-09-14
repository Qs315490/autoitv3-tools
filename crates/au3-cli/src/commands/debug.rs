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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::rc::Rc;

use autoitv3_ast::span::Span;
use autoitv3_ast::Program;
use autoitv3_runtime::debug::{Breakpoint, DebugAction, DebugHost, Debugger, StopReason};
use autoitv3_runtime::RuntimeError;
use autoitv3_runtime::Runtime;
use clap::Args;

use crate::args::{
    load_program, CliError, CliResult, EffectArgs, ProfileArgs, StepArgs, WinEmuArgs,
};
use crate::output::format_value;
use std::path::Path;

/// Arguments for `au3 debug`.
#[derive(Args, Debug)]
pub struct DebugArgs {
    /// Input AutoIt v3 script
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

    #[command(flatten)]
    pub win: WinEmuArgs,
}

/// Entry point for the `debug` subcommand.
pub fn run(args: &DebugArgs) -> CliResult<()> {
    let prog = load_program(&args.input)?;
    let source = std::fs::read_to_string(&args.input)
        .map_err(|e| CliError::io(format!("cannot read {}: {e}", args.input)))?;

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
    let mut rt = build_runtime(&prog, args, shell.clone());

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
            rt = build_runtime(&prog, args, shell.clone());
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
fn build_runtime(prog: &Program, args: &DebugArgs, shell: Rc<RefCell<Shell>>) -> Runtime {
    let mut rt = Runtime::with_program(prog);
    match args.win.platform(Some(Path::new(&args.input))) {
        Ok(platform) => rt.set_platform(platform),
        Err(e) => eprintln!("warning: {}", e.message),
    }
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
    /// Stop at the very next statement, wherever it is.
    Step,
    /// Stop at the next statement in this frame or a shallower one.
    Over(usize),
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
        Self {
            script,
            lines: source.lines().map(|l| l.to_string()).collect(),
            queue,
            show_prompts: std::io::stdin().is_terminal(),
            finished: false,
            step: StepMode::Run,
            stop_at_start: args.stop_at_start,
            current: None,
            paused: false,
            tracing: false,
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
        self.step = if self.stop_at_start { StepMode::Step } else { StepMode::Run };
    }

    /// Report how the script body ended.
    fn report_run(&mut self, outcome: &Result<autoitv3_runtime::Flow, autoitv3_runtime::RuntimeError>) {
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
    fn next_command(&mut self) -> Option<String> {
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
            print!("{}", self.prompt());
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
            "s" | "step" => self.step_command("step"),
            "n" | "next" => self.step_command("next"),
            "fin" | "finish" => self.step_command("finish"),
            // gdb's `until` collapses into the one-shot breakpoint: with no
            // frame-boundary semantics it was a duplicate of `tbreak <line>`.
            "until" | "u" => self.tbreak_command(rest.trim(), host),
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
            "bt" | "where" | "backtrace" | "w" => {
                self.backtrace(host);
                Outcome::Stay
            }
            "l" | "list" => {
                self.list_command(rest.trim());
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

    /// `step`, `next` and `finish` differ only in when they stop.
    fn step_command(&mut self, which: &str) -> Outcome {
        self.step = match (self.current, self.paused, which) {
            (Some((_, depth)), true, "next") => StepMode::Over(depth),
            (Some((_, depth)), true, "finish") => StepMode::Out(depth),
            _ => StepMode::Step,
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
        let mut spec = if let Ok(line) = pos.parse::<u32>() {
            if line == 0 || line as usize > self.lines.len().max(1) {
                println!(
                    "line {line} is outside {} (1..{})",
                    self.script,
                    self.lines.len()
                );
                return None;
            }
            BpSpec::line(line)
        } else {
            // Not a number: a function name. Stop at its first statement.
            match host.function_entry_line(pos) {
                Some(line) => {
                    let mut spec = BpSpec::line(line);
                    spec.label = Some(format!("func {pos}"));
                    spec
                }
                None => {
                    println!("no such line or function: {pos:?}");
                    return None;
                }
            }
        };
        spec.condition = condition;
        spec.skip = skip;
        spec.every = every;
        spec.stop = stop;
        if let Some(a) = action {
            spec.actions.push(a);
        }
        Some(spec)
    }

    /// `jmp <line>` — unconditionally transfer execution: statements between
    /// here and the target are skipped without running. Only lines of the
    /// frame that will resume are valid; the interpreter rejects the rest.
    fn jmp_command(&mut self, rest: &str, host: &mut dyn DebugHost) -> Outcome {
        if !self.paused {
            println!("jmp needs a stopped run — use run first");
            return Outcome::Stay;
        }
        match rest.trim().parse::<u32>() {
            Ok(line) => match host.jump_to(line) {
                Ok(()) => {
                    println!("jumping to line {line}");
                    Outcome::Resume
                }
                Err(e) => {
                    println!("cannot jump: {e}");
                    Outcome::Stay
                }
            },
            Err(_) => {
                println!("usage: jmp <line> (a statement line of the current frame)");
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
        // the command that assigns, and it says so.
        self.show(host.evaluate_expression(expr));
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
        let Some(frame) = frames.last() else {
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
        if frames.is_empty() {
            // Top-level code: there is no activation record to show, but the
            // position still is one.
            match self.current {
                Some((span, _)) => {
                    println!("#0  <script> at {}:{}", span.start.line, span.start.col)
                }
                None => println!("#0  <script>"),
            }
            return;
        }
        // Innermost first, numbered from zero, the way gdb prints a stack.
        for (i, frame) in frames.iter().rev().enumerate() {
            let name = frame.function.as_deref().unwrap_or("<script>");
            let at = frame
                .span
                .map(|s| format!("{}:{}", s.start.line, s.start.col))
                .unwrap_or_else(|| "-".to_string());
            println!("#{i}  {name} at {at}");
        }
    }

    /// `list [line|+]`, showing a window around the stop point.
    fn list_command(&mut self, rest: &str) {
        if self.lines.is_empty() {
            println!("{} is empty", self.script);
            return;
        }
        let current = self.current.map(|(span, _)| span.start.line);
        let centre = match rest.trim() {
            "" => current.unwrap_or(1),
            "+" => current.map(|l| l.saturating_add(1)).unwrap_or(1),
            other => match other.parse::<u32>() {
                Ok(line) => line,
                Err(_) => {
                    println!("usage: list [line]");
                    return;
                }
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

    fn trace_command(&mut self, rest: &str) {
        match rest {
            "on" => {
                self.tracing = true;
                println!("statement tracing on");
            }
            "off" => {
                self.tracing = false;
                println!("statement tracing off");
            }
            "" => println!(
                "statement tracing is {}",
                if self.tracing { "on" } else { "off" }
            ),
            other => println!("usage: trace on|off (not {other:?})"),
        }
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
  step, s                run the next statement, entering calls
  next, n                run to the next statement in this frame
  finish, fin            run until the current function returns
  until <line>           alias of `tbreak <line>`
  break <line> [if E]    set a breakpoint, optionally conditional
  delete [id]            remove one breakpoint, or all of them
  enable/disable <id>    toggle a breakpoint
  print <expr>, p        evaluate an expression in the current frame
  set $x = <expr>        assign to a variable in the current frame
  info <topic>           breakpoints | locals | globals | functions | frame
  backtrace, bt          show the call stack
  list [line], l         show source around the stop point
  trace on|off           echo every statement as it runs
  catch on|off           stop where an uncaught error is raised (default on)
  source <file>          run the commands in a file, then come back here
  quit, q                leave the session

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
        if self.tracing {
            println!("[trace] {}:{} depth={depth}", span.start.line, span.start.col);
        }
        let stop = match self.step {
            StepMode::Run => false,
            StepMode::Step => true,
            StepMode::Over(d) => depth <= d,
            StepMode::Out(d) => depth < d,
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
            StopReason::Step | StopReason::Pause => {
                self.paused = true;
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
        let cmd = self.next_command()?;
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
                match self.next_command() {
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
                    match self.next_command() {
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

/// Split `"break 12"` into `("break", "12")`.
fn split_command(line: &str) -> (String, String) {
    match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word.to_ascii_lowercase(), rest.to_string()),
        None => (line.to_ascii_lowercase(), String::new()),
    }
}

/// One-line help for a single command.
fn help_for(topic: &str) -> String {
    match topic {
        "run" | "r" => "run — start the script body; typing it again starts over".to_string(),
        "continue" | "c" => "continue — resume until the next breakpoint".to_string(),
        "step" | "s" => "step — run one statement, entering function calls".to_string(),
        "next" | "n" => {
            "next — run until the next statement in this frame or shallower".to_string()
        }
        "finish" => "finish — run until the current function returns".to_string(),
        "until" | "u" => "until <line> — alias of `tbreak <line>`".to_string(),
        "break" | "b" => {
            "break <line|func> [if <expr>] [skip <n>] [every <n>] [nostop] [do <cmd>] — stop there; do runs debugger commands on hit".to_string()
        }
        "commands" => {
            "commands <id> [do <cmd> | off] — on-hit debugger commands (run even with nostop)".to_string()
        }
        "jmp" | "j" => {
            "jmp <line> — unconditionally jump: skip statements up to the target line".to_string()
        }
        "tbreak" | "tb" => {
            "tbreak <line|func> — one-shot breakpoint: run until it is reached".to_string()
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
        "backtrace" | "bt" | "where" => "backtrace — the call stack, innermost last".to_string(),
        "list" | "l" => "list [line] — eight source lines around the stop point".to_string(),
        "trace" => "trace on|off — echo every statement as it executes".to_string(),
        "catch" => {
            "catch on|off — stop where an uncaught error is raised, before it unwinds".to_string()
        }
        "source" => "source <file> — queue the commands in a file, one per line".to_string(),
        "quit" | "q" => "quit — leave the session".to_string(),
        other => format!("no help for {other:?}"),
    }
}
