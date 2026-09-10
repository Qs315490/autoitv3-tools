//! `au3 run <FUNC> <FILE> [--arg V]... [--init] [--trace]` — interpret.
//!
//! Calls one function on [`Runtime`](autoitv3_runtime::Runtime). This is how the
//! obfuscator's table builders can be probed directly, and `--trace` wires in a
//! [`Debugger`](autoitv3_runtime::debug::Debugger) to show the statement stream
//! the future debug module consumes.

use autoitv3_runtime::debug::{DebugAction, Debugger, StopReason};
use autoitv3_runtime::{ExecutionProfile, Value};
use clap::Args;

use crate::args::{load_program, parse_arg_value, CliError, CliResult};

/// Arguments for `au3 run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Function to call
    #[arg(value_name = "FUNC")]
    pub function: String,

    /// Input AutoIt v3 script
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

    /// Run with AutoIt semantics: really wait in Sleep(), really randomise
    /// Random(), and let file/environment writes happen.
    ///
    /// The default is the deterministic analysis profile, which is fast,
    /// reproducible and refuses writes (see `ExecutionProfile`).
    #[arg(long)]
    pub faithful: bool,
}

/// Entry point for the `run` subcommand.
pub fn run(args: &RunArgs) -> CliResult<()> {
    let prog = load_program(&args.input)?;
    // Install the platform layer for this OS so OS-specific builtins can be
    // reached (see `autoitv3-platform`).
    let mut rt = autoitv3_platform::runtime_with_platform(&prog);
    // Probing a script wants reproducibility and no side effects; `--faithful`
    // switches to AutoIt's own semantics instead.
    rt.set_profile(if args.faithful {
        ExecutionProfile::faithful()
    } else {
        ExecutionProfile::deterministic()
    });

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

/// Render a runtime value for display, summarising arrays.
fn format_value(v: &Value) -> String {
    match v {
        Value::Array(a) => {
            let a = a.borrow();
            let preview: Vec<String> = a
                .iter()
                .take(6)
                .map(|x| format!("{:?}", x.to_autoit_string()))
                .collect();
            let more = if a.len() > preview.len() { ", ..." } else { "" };
            format!("Array[{}] {{{}{}}}", a.len(), preview.join(", "), more)
        }
        other => format!("{other:?}"),
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
    fn on_statement(&mut self, span: autoitv3_ast::span::Span, depth: usize) -> DebugAction {
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

    fn on_stop(&mut self, reason: &StopReason) {
        eprintln!("[trace] stop: {reason:?}");
    }
}