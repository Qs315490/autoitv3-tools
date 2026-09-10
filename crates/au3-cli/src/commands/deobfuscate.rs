//! `au3 deobfuscate <FILE> [-o FILE] [--no-rename]` — run the deobfuscation
//! pipeline.
//!
//! Pipeline (see `autoitv3-deobf`): constant folding, function-table
//! resolution (`$fn_table[0x..](...)` → `FuncName(...)`), indirect-call
//! simplification (`Call("Foo", ...)` → `Foo(...)`) and deterministic
//! identifier renaming. Comments are stripped, since they are noise once the
//! code has been rewritten. A summary goes to stderr so stdout stays a clean
//! AutoIt program.
//!
//! Renaming is optional (`--no-rename`): with it off, only the structure is
//! rewritten and every original variable/function name is preserved.

use autoitv3_deobf::{deobfuscate, evaluate_with_platform, Deobfuscator};
use autoitv3_format::PrettyPrinter;
use clap::Args;

use crate::args::{load_program, CliResult, OutputArgs, WinEmuArgs};
use crate::output::write_output;

/// Arguments for `au3 deobfuscate`.
#[derive(Args, Debug)]
pub struct DeobfuscateArgs {
    /// Input AutoIt v3 script
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Run the script body first and inline the table values it computed,
    /// then apply the syntactic passes
    #[arg(long)]
    pub evaluate: bool,

    /// With --evaluate, run with AutoIt semantics instead of the
    /// deterministic analysis profile
    #[arg(long)]
    pub faithful: bool,

    /// Keep the original variable and function names: skip the rename pass,
    /// leaving only constant folding and function-table resolution
    #[arg(long)]
    pub no_rename: bool,

    #[command(flatten)]
    pub win: WinEmuArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `deobfuscate` subcommand.
pub fn run(args: &DeobfuscateArgs) -> CliResult<()> {
    let mut prog = load_program(&args.input)?;

    // Runtime evaluation first: it recovers values the syntactic passes cannot
    // see (the obfuscator's string table), and the later passes then fold and
    // rename the result.
    if args.evaluate {
        let outcome = evaluate_with_platform(
            &mut prog,
            super::evaluate::profile(args.faithful),
            args.win.platform()?,
        );
        super::evaluate::report(&outcome);
    }

    let report = if args.no_rename {
        Deobfuscator::without_rename().run(&mut prog)
    } else {
        deobfuscate(&mut prog)
    };
    eprintln!(
        "deobfuscated: {} folds, {} vars, {} funcs renamed{}; \
         table: {} entries, {} calls, {} refs; {} indirect calls simplified",
        report.folds,
        report.renamed.vars,
        report.renamed.funcs,
        if args.no_rename { " (renaming disabled)" } else { "" },
        report.table.entries,
        report.table.calls,
        report.table.refs,
        report.simplified.calls + report.simplified.executes,
    );

    let mut printer = PrettyPrinter::new().strip_comments(true);
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}