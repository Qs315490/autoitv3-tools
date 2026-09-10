//! `au3 deobfuscate <FILE> [-o FILE]` — run the deobfuscation pipeline.
//!
//! Pipeline (see `autoitv3-deobf`): constant folding, function-table
//! resolution (`$fn_table[0x..](...)` → `FuncName(...)`) and deterministic
//! identifier renaming. Comments are stripped, since they are noise once the
//! code has been rewritten. A summary goes to stderr so stdout stays a clean
//! AutoIt program.

use autoitv3_deobf::deobfuscate;
use autoitv3_format::PrettyPrinter;
use clap::Args;

use crate::args::{load_program, CliResult, OutputArgs};
use crate::output::write_output;

/// Arguments for `au3 deobfuscate`.
#[derive(Args, Debug)]
pub struct DeobfuscateArgs {
    /// Input AutoIt v3 script
    #[arg(value_name = "FILE")]
    pub input: String,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `deobfuscate` subcommand.
pub fn run(args: &DeobfuscateArgs) -> CliResult<()> {
    let mut prog = load_program(&args.input)?;

    let report = deobfuscate(&mut prog);
    eprintln!(
        "deobfuscated: {} folds, {} vars, {} funcs, {} macros renamed; \
         table: {} entries, {} calls, {} refs rewritten",
        report.folds,
        report.renamed.vars,
        report.renamed.funcs,
        report.renamed.macros,
        report.table.entries,
        report.table.calls,
        report.table.refs,
    );

    let mut printer = PrettyPrinter::new().strip_comments(true);
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}