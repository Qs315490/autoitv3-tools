//! `au3 pretty <file.au3> [-o FILE]` — re-print the script in normalised form.
//!
//! Comments are **preserved** (the default of
//! [`PrettyPrinter`](autoitv3_format::PrettyPrinter)); use `au3 deobfuscate`
//! when you want them stripped. Only whitespace and layout are normalised, so
//! the output is a faithful, re-parseable rendering of the input.

use autoitv3_format::PrettyPrinter;

use crate::args::{load_program, parse_in_out, CliResult};
use crate::output::write_output;

/// Entry point for the `pretty` subcommand.
pub fn run(args: &[String]) -> CliResult<()> {
    let io = parse_in_out("pretty", args)?;
    let prog = load_program(&io.input)?;

    let mut printer = PrettyPrinter::new();
    let rendered = printer.print_program(&prog);

    write_output(io.output.as_deref(), &rendered)
}