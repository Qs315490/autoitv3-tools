//! `au3 run <FUNC> <FILE> [--arg V]... [--init] [--trace]` — interpret.
//!
//! Calls one function on [`Runtime`](autoitv3_runtime::Runtime). This is how the
//! obfuscator's table builders can be probed directly, and `--trace` wires in a
//! [`Debugger`](autoitv3_runtime::debug::Debugger) to show the statement stream
//! the future debug module consumes.

use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason};
use autoitv3_runtime::{Runtime, Value};
use clap::Args;

use crate::args::{
    load_input, parse_arg_value, CliError, CliResult, EffectArgs, ProfileArgs, StepArgs,
    WinEmuArgs,
};
use crate::output::format_value;
use std::path::Path;

/// Arguments for `au3 run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Function to call
    #[arg(value_name = "FUNC")]
    pub function: String,

    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Argument to pass to the function; repeat for more.
    /// Integers (decimal, or `0x` hex) are passed as numbers, anything else
    /// as a string.
    #[arg(long = "arg", value_name = "VALUE")]
    pub args: Vec<String>,

    /// Run the top-level script first, so global tables (`$fn_table`, ...) exist
    #[arg(long)]
    pub init: bool,

    /// Print the executed statement stream to stderr
    #[arg(long)]
    pub trace: bool,

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

/// Entry point for the `run` subcommand.
pub fn run(args: &RunArgs) -> CliResult<()> {
    let input = load_input(&args.input)?;
    let prog = input.program;
    // Install the platform layer for this OS so OS-specific builtins can be
    // reached (see `autoitv3-platform`); off Windows the Windows emulation
    // layer answers first, with the version these arguments select.
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(args.win.platform(
        Some(Path::new(&args.input)),
        input.resource_module.as_deref(),
    )?);
    rt.set_max_steps(args.steps.max_steps);
    // Probing a script wants reproducibility and no side effects; `--faithful`
    // switches to AutoIt's own semantics instead. `--allow`/`--deny` then
    // fine-tune individual effects on top of either preset.
    rt.set_profile(args.effects.apply(args.profile.profile())?);

    if args.trace {
        rt.set_debugger(Box::new(TracePrinter::new()));
    }
    if args.init {
        // Execute the top-level script so the tables the function may rely on
        // exist before it runs.
        if let Err(e) = rt.run_script() {
            return Err(CliError::failure(format!(
                "error while running script body: {e}"
            )));
        }
    }

    let call_args: Vec<Value> = args.args.iter().map(|a| parse_arg_value(a)).collect();

    match rt.call_function(&args.function, call_args) {
        Ok(value) => {
            println!("{}() = {}", args.function, format_value(&value));
            if let Some(reason) = rt.take_pause() {
                eprintln!("(stopped: {reason:?})");
            }
            Ok(())
        }
        Err(e) => Err(CliError::failure(format!(
            "runtime error in {}(): {e}",
            args.function
        ))),
    }
}

/// A debugger that prints the statement stream to stderr.
///
/// A deliberately thin demonstration of the [`Debugger`] interface: the
/// interpreter hands it every statement span and it decides what to surface.
struct TracePrinter {
    statements: u64,
    calls: Vec<String>,
}

impl TracePrinter {
    fn new() -> Self {
        Self { statements: 0, calls: Vec::new() }
    }
}

impl Debugger for TracePrinter {
    fn on_statement(
        &mut self,
        span: autoitv3_ast::span::Span,
        depth: usize,
        _host: &mut dyn DebugHost,
    ) -> DebugAction {
        self.statements += 1;
        // Keep the output usable on a 23k-line script.
        if self.statements <= 40 {
            eprintln!(
                "[trace] {:>5}:{:<4} depth={depth}",
                span.start.line, span.start.col
            );
        } else if self.statements == 41 {
            eprintln!("[trace] ... (further statements suppressed)");
        }
        DebugAction::Continue
    }

    fn on_call_enter(&mut self, name: &str, args: &[Value]) {
        self.calls.push(name.to_string());
        if self.calls.len() <= 40 {
            eprintln!("[trace] call {name}({} args)", args.len());
        }
    }

    fn on_stop(&mut self, reason: &StopReason, _host: &mut dyn DebugHost) {
        eprintln!("[trace] stop: {reason:?}");
    }
}