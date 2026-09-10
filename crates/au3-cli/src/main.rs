//! Command line entry point for the AutoIt v3 analysis tool.
//!
//! Usage:
//!   au3 <file.au3>                    # parse, report stats
//!   au3 --pretty <file.au3>           # parse then pretty-print (normalize)
//!   au3 --deobfuscate <file.au3>      # constant-fold + rename, then pretty-print
//!   au3 --pretty -o out.au3 <file>    # write output to a file
//!   au3 --pretty -o - <file>          # write output to stdout (explicit)
//!   au3 --run <Func> <file.au3>       # interpret: call Func() and print the result
//!   au3 --run <Func> --arg 2 --arg 3 <file.au3>   # ... passing arguments
//!   au3 --run <Func> --trace <file>   # ... while tracing executed statements
//!   au3 --run <Func> --init <file>    # run the script body first (sets globals)
//!
//! `-o FILE` redirects the formatted output to FILE. `-o -` (or omitting `-o`)
//! writes to stdout, so the original input file is never modified.
//!
//! `--run` exercises `autoitv3-runtime`, which is what lets the deobfuscator
//! evaluate the obfuscator's table builders; `--trace` attaches a debugger to
//! show the same statement-level stream the future debug module consumes.

use std::io::Write;
use autoitv3_ast::{parse, Program};
use autoitv3_format::PrettyPrinter;
use autoitv3_deobf::deobfuscate;
use autoitv3_runtime::debug::{DebugAction, Debugger, StopReason};
use autoitv3_runtime::{Runtime, Value};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut pretty = false;
    let mut deobfuscate_flag = false;
    let mut out_file: Option<String> = None;
    let mut file: Option<String> = None;
    let mut run_func: Option<String> = None;
    let mut run_args: Vec<Value> = Vec::new();
    let mut trace = false;
    let mut init = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--pretty" => pretty = true,
            "--deobfuscate" => deobfuscate_flag = true,
            "--trace" => trace = true,
            "--init" => init = true,
            "--run" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --run requires a function name");
                    std::process::exit(2);
                }
                run_func = Some(args[i].clone());
            }
            "--arg" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --arg requires a value");
                    std::process::exit(2);
                }
                run_args.push(parse_arg(&args[i]));
            }
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: -o requires a FILE argument (use '-' for stdout)");
                    std::process::exit(2);
                }
                out_file = Some(args[i].clone());
            }
            s if s.starts_with('-') => {
                eprintln!("unknown option: {s}");
                std::process::exit(2);
            }
            s => {
                if file.is_some() {
                    eprintln!("error: multiple input files given");
                    std::process::exit(2);
                }
                file = Some(s.to_string());
            }
        }
        i += 1;
    }

    let Some(file) = file else {
        eprintln!("usage: au3 [--pretty | --deobfuscate] [-o FILE] <file.au3>");
        eprintln!("       use -o - to redirect output to stdout explicitly");
        std::process::exit(2);
    };

    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {file}: {e}");
            std::process::exit(2);
        }
    };

    let mut prog: Program = match parse(&src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };

    // `--run`: interpret a function instead of transforming the source.
    if let Some(name) = run_func {
        let mut rt = Runtime::with_program(&prog);
        if trace {
            rt.set_debugger(Box::new(TracePrinter::new()));
        }
        if init {
            // Execute the top-level script so the obfuscator's global tables
            // (`$fn_table`, ...) exist before the requested function runs.
            if let Err(e) = rt.run_script() {
                eprintln!("error while running script body: {e}");
                std::process::exit(1);
            }
        }
        match rt.call_function(&name, run_args.clone()) {
            Ok(v) => {
                println!("{name}() = {}", format_value(&v));
                if let Some(reason) = rt.take_pause() {
                    eprintln!("(stopped: {reason:?})");
                }
            }
            Err(e) => {
                eprintln!("runtime error in {name}(): {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if deobfuscate_flag {
        let report = deobfuscate(&mut prog);
        eprintln!(
            "deobfuscated: {} folds, {} vars, {} funcs, {} macros renamed; table: {} entries, {} calls, {} refs rewritten",
            report.folds, report.renamed.vars, report.renamed.funcs, report.renamed.macros,
            report.table.entries, report.table.calls, report.table.refs
        );
    }

    let mut pp = PrettyPrinter::new().strip_comments(deobfuscate_flag);
    let out = pp.print_program(&prog);

    if pretty || deobfuscate_flag {
        match out_file.as_deref() {
            Some("-") | None => {
                print!("{out}");
            }
            Some(path) => {
                if let Err(e) = std::fs::write(path, &out) {
                    eprintln!("error writing {path}: {e}");
                    std::process::exit(2);
                }
            }
        }
    } else {
        // Plain parse mode: only stats, never emits source output.
        let funcs = count_funcs(&prog);
        println!(
            "parsed OK: {} top-level items, {} functions",
            prog.items.len(),
            funcs
        );
        let _ = std::io::stdout().flush();
    }
}

fn count_funcs(prog: &Program) -> usize {
    use autoitv3_ast::ast::ItemKind;
    prog.items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count()
}

/// Parse a `--arg` value: integers become `Int`, everything else a string.
fn parse_arg(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Int(i);
    }
    if raw.starts_with("0x") || raw.starts_with("0X") {
        if let Ok(i) = i64::from_str_radix(&raw[2..], 16) {
            return Value::Int(i);
        }
    }
    Value::Str(raw.to_string())
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
        other => format!("{:?}", other),
    }
}

/// A debugger that prints the statement stream to stderr.
///
/// This is a thin demonstration of the [`Debugger`] interface the future debug
/// module builds on: the interpreter hands it every statement span, and it
/// decides what to surface.
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

    fn on_stop(&mut self, reason: &StopReason) {
        eprintln!("[trace] stop: {reason:?}");
    }
}
