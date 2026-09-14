//! `au3 pretty <FILE> [-o FILE]` — re-print the script in normalised form.
//!
//! Comments are **preserved** (the default of
//! [`PrettyPrinter`](autoitv3_format::PrettyPrinter)); use `au3 deobfuscate`
//! when you want them stripped. Only whitespace and layout are normalised, so
//! the output is a faithful, re-parseable rendering of the input.

use autoitv3_format::PrettyPrinter;
use clap::Args;

use crate::args::{load_program, CliResult, OutputArgs};
use crate::output::write_output;

/// Arguments for `au3 pretty`.
#[derive(Args, Debug)]
pub struct PrettyArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `pretty` subcommand.
pub fn run(args: &PrettyArgs) -> CliResult<()> {
    let prog = load_program(&args.input)?;

    let mut printer = PrettyPrinter::new();
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}