//! `au3 run <Func> <file.au3> [--arg V]... [--init] [--trace]` — interpret.
//!
//! Calls one function on [`Runtime`](autoitv3_runtime::Runtime). This is how the
//! obfuscator's table builders can be probed directly, and `--trace` wires in a
//! [`Debugger`](autoitv3_runtime::debug::Debugger) to show the statement stream
//! the future debug module consumes.

use autoitv3_runtime::debug::{DebugAction, Debugger, StopReason};
use autoitv3_runtime::{Runtime, Value};

use crate::args::{load_program, parse_arg_value, CliError, CliResult};

/// Entry point for the `run` subcommand.
pub fn run(args: &[String]) -> CliResult<()> {
    let mut func: Option<String> = None;
    let mut input: Option<String> = None;
    let mut call_args: Vec<Value> = Vec::new();
    let mut init = false;
    let mut trace = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--arg" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::usage("run: --arg requires a value")
                })?;
                call_args.push(parse_arg_value(raw));
            }
            "--init" => init = true,
            "--trace" => trace = true,
            s if s.starts_with('-') && s != "-" => {
                return Err(CliError::usage(format!("run: unknown option `{s}`")));
            }
            s => {
                // First positional is the function, second the input file.
                if func.is_none() {
                    func = Some(s.to_string());
                } else if input.is_none() {
                    input = Some(s.to_string());
                } else {
                    return Err(CliError::usage("run: unexpected extra argument"));
                }
            }
        }
        i += 1;
    }

    let func = func
        .ok_or_else(|| CliError::usage("run: missing function name (usage: au3 run <Func> <file.au3>)"))?;
    let input =
        input.ok_or_else(|| CliError::usage("run: missing input file (usage: au3 run <Func> <file.au3>)"))?;

    let prog = load_program(&input)?;
    let mut rt = Runtime::with_program(&prog);

    if trace {
        rt.set_debugger(Box::new(TracePrinter::new()));
    }
    if init {
        // Execute the top-level script so global tables the function may rely
        // on (`$fn_table`, ...) exist before it runs.
        if let Err(e) = rt.run_script() {
            return Err(CliError::failure(format!("error while running script body: {e}")));
        }
    }

    match rt.call_function(&func, call_args) {
        Ok(value) => {
            println!("{func}() = {}", format_value(&value));
            if let Some(reason) = rt.take_pause() {
                eprintln!("(stopped: {reason:?})");
            }
            Ok(())
        }
        Err(e) => Err(CliError::failure(format!("runtime error in {func}(): {e}"))),
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