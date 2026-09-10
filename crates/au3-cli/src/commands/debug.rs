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
use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason};
use autoitv3_runtime::{ExecutionProfile, Runtime};
use clap::Args;

use crate::args::{load_program, CliError, CliResult, WinEmuArgs};
use crate::output::format_value;

/// Arguments for `au3 debug`.
#[derive(Args, Debug)]
pub struct DebugArgs {
    /// Input AutoIt v3 script
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Command to run at startup; repeat for a whole session.
    ///
    /// Commands are accepted wherever a prompt would appear, including at a
    /// breakpoint, so `-c run -c next -c quit` walks the script.
    #[arg(short = 'c', long = "command", value_name = "CMD")]
    pub commands: Vec<String>,

    /// Stop on the first statement the script executes, as if `step` had been
    /// typed before `run`
    #[arg(long)]
    pub stop_at_start: bool,

    /// Run with AutoIt semantics: really wait in Sleep(), really randomise
    /// Random(), and let file/environment writes happen.
    ///
    /// The default is the deterministic analysis profile, which is fast,
    /// reproducible and refuses writes (see `ExecutionProfile`).
    #[arg(long)]
    pub faithful: bool,

    #[command(flatten)]
    pub win: WinEmuArgs,
}

/// Entry point for the `debug` subcommand.
pub fn run(args: &DebugArgs) -> CliResult<()> {
    let prog = load_program(&args.input)?;
    let source = std::fs::read_to_string(&args.input)
        .map_err(|e| CliError::io(format!("cannot read {}: {e}", args.input)))?;

    let shell = Rc::new(RefCell::new(Shell::new(args.input.clone(), &source, args)));
    let mut rt = build_runtime(&prog, args, shell.clone());

    // The outer loop. A `Resume` here means "start the script body"; the same
    // answer at a breakpoint means "give control back to the interpreter",
    // which is what [`Shell::prompt_loop`] does with it.
    while !shell.borrow().finished {
        // The borrow must end before the run: the runtime calls back into the
        // shell, and holding it here would silence every callback.
        let Some(cmd) = shell.borrow_mut().next_command() else {
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
    match args.win.platform() {
        Ok(platform) => rt.set_platform(platform),
        Err(e) => eprintln!("warning: {}", e.message),
    }
    rt.set_profile(if args.faithful {
        ExecutionProfile::faithful()
    } else {
        ExecutionProfile::deterministic()
    });
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

    fn on_stop(&mut self, reason: &StopReason, host: &mut dyn DebugHost) {
        if let Ok(mut shell) = self.0.try_borrow_mut() {
            shell.on_stop(reason, host);
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
    /// Stop when this source line is reached again.
    Until(u32),
}

/// A breakpoint edit a command recorded, waiting for the host to apply it.
///
/// Commands that read the program take the host directly; the ones that *edit*
/// it are written as a small pending action so that the command parser stays
/// free of the host, and applied in one place at the end of [`Shell::execute`].
enum Edit {
    Add(u32, Option<String>),
    Remove(Option<u32>),
    Enable(u32, bool),
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
    /// A breakpoint edit for [`Shell::flush_edits`] to apply.
    pending: Option<Edit>,
    /// `run` was typed at a stop, so the current run should unwind and start
    /// over rather than simply resuming.
    restart: bool,
    /// Breakpoint specs remembered so `run` can restart with them intact.
    saved_breakpoints: Vec<(u32, Option<String>)>,
}

impl Shell {
    fn new(script: String, source: &str, args: &DebugArgs) -> Self {
        Self {
            script,
            lines: source.lines().map(|l| l.to_string()).collect(),
            queue: args.commands.iter().cloned().collect(),
            show_prompts: std::io::stdin().is_terminal(),
            finished: false,
            step: StepMode::Run,
            stop_at_start: args.stop_at_start,
            current: None,
            paused: false,
            tracing: false,
            pending: None,
            restart: false,
            saved_breakpoints: Vec::new(),
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
        for (line, condition) in specs {
            host.add_breakpoint(line, condition);
        }
    }

    /// Note that a run is starting.
    fn begin_run(&mut self) {
        self.current = None;
        self.paused = false;
        self.restart = false;
        self.step = if self.stop_at_start { StepMode::Step } else { StepMode::Run };
    }

    /// Report how the script body ended.
    fn report_run(&mut self, outcome: &Result<autoitv3_runtime::Flow, autoitv3_runtime::RuntimeError>) {
        if self.finished {
            return;
        }
        match outcome {
            Ok(_) => println!("[script finished]"),
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
            "until" | "u" => self.until_command(rest.trim()),
            "b" | "break" => self.break_command(rest.trim()),
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

    fn until_command(&mut self, rest: &str) -> Outcome {
        let Ok(line) = rest.parse::<u32>() else {
            println!("usage: until <line>");
            return Outcome::Stay;
        };
        if !self.paused {
            println!("the script is not stopped; use `run` first");
            return Outcome::Stay;
        }
        self.step = StepMode::Until(line);
        Outcome::Resume
    }

    fn break_command(&mut self, rest: &str) -> Outcome {
        let (line, condition) = match rest.split_once(" if ") {
            Some((line, cond)) => (line.trim(), Some(cond.trim().to_string())),
            None => match rest.split_once("if ") {
                Some((line, cond)) => (line.trim(), Some(cond.trim().to_string())),
                None => (rest.trim(), None),
            },
        };
        let Ok(line) = line.parse::<u32>() else {
            println!("usage: break <line> [if <expr>]");
            return Outcome::Stay;
        };
        if line == 0 || line as usize > self.lines.len().max(1) {
            println!(
                "line {line} is outside {} (1..{})",
                self.script,
                self.lines.len()
            );
            return Outcome::Stay;
        }
        self.pending = Some(Edit::Add(line, condition));
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
            Some(Edit::Add(line, condition)) => {
                let id = host.add_breakpoint(line, condition.clone());
                match condition {
                    Some(c) => println!("Breakpoint {id} at line {line} if {c}"),
                    None => println!("Breakpoint {id} at line {line}"),
                }
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
        self.saved_breakpoints = host
            .breakpoints()
            .iter()
            .map(|b| (b.line, b.condition.clone()))
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
            let condition = match &bp.condition {
                Some(c) => format!(" if {c}"),
                None => String::new(),
            };
            println!(
                "{:>3}  line {:<6} enabled={state}  hits={}{condition}",
                bp.id, bp.line, bp.hits
            );
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
  until <line>           run until that line is reached
  break <line> [if E]    set a breakpoint, optionally conditional
  delete [id]            remove one breakpoint, or all of them
  enable/disable <id>    toggle a breakpoint
  print <expr>, p        evaluate an expression in the current frame
  set $x = <expr>        assign to a variable in the current frame
  info <topic>           breakpoints | locals | globals | functions | frame
  backtrace, bt          show the call stack
  list [line], l         show source around the stop point
  trace on|off           echo every statement as it runs
  quit, q                leave the session"
        );
    }
}

impl Debugger for Shell {
    fn on_statement(&mut self, span: Span, depth: usize, _host: &mut dyn DebugHost) -> DebugAction {
        // Unwind the run when the session is over, or when `run` asked for a
        // fresh one from inside a stop.
        if self.finished || self.restart {
            return DebugAction::Abort;
        }
        if self.tracing {
            println!("[trace] {}:{} depth={depth}", span.start.line, span.start.col);
        }
        let stop = match self.step {
            StepMode::Run => false,
            StepMode::Step => true,
            StepMode::Over(d) => depth <= d,
            StepMode::Out(d) => depth < d,
            StepMode::Until(line) => span.start.line == line,
        };
        self.current = Some((span, depth));
        if stop {
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
                println!("Breakpoint {id}, line {line}");
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
            let Some(cmd) = self.next_command() else {
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
        "until" => "until <line> — run until that source line is reached".to_string(),
        "break" | "b" => {
            "break <line> [if <expr>] — stop there; the condition is AutoIt source".to_string()
        }
        "print" | "p" => {
            "print <expr> — evaluate in the stopped frame (`p $x`, `p $a[2]`)".to_string()
        }
        "set" => "set $var = <expr> — assign in the stopped frame".to_string(),
        "info" | "i" => "info breakpoints|locals|globals|functions|frame".to_string(),
        "backtrace" | "bt" | "where" => "backtrace — the call stack, innermost last".to_string(),
        "list" | "l" => "list [line] — eight source lines around the stop point".to_string(),
        "trace" => "trace on|off — echo every statement as it executes".to_string(),
        "quit" | "q" => "quit — leave the session".to_string(),
        other => format!("no help for {other:?}"),
    }
}
