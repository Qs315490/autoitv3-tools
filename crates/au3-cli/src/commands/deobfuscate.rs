//! `au3 deobfuscate <file.au3> [-o FILE]` — run the deobfuscation pipeline.
//!
//! Pipeline (see `autoitv3-deobf`): constant folding, function-table
//! resolution (`$fn_table[0x..](...)` -> `FuncName(...)`) and deterministic
//! identifier renaming. Comments are stripped, since they are noise once the
//! code has been rewritten. A summary goes to stderr so that stdout stays a
//! clean AutoIt program.

use autoitv3_deobf::deobfuscate;
use autoitv3_format::PrettyPrinter;

use crate::args::{load_program, parse_in_out, CliResult};
use crate::output::write_output;

/// Entry point for the `deobfuscate` subcommand.
pub fn run(args: &[String]) -> CliResult<()> {
    let io = parse_in_out("deobfuscate", args)?;
    let mut prog = load_program(&io.input)?;

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

    write_output(io.output.as_deref(), &rendered)
}