//! `au3 parse <FILE>` — parse the script and report its shape.
//!
//! The cheapest command: it proves the file is syntactically valid AutoIt v3
//! and summarises what was found, without emitting any source.

use autoitv3_ast::ast::ItemKind;
use clap::Args;

use crate::args::{load_program, CliResult};

/// Arguments for `au3 parse`.
#[derive(Args, Debug)]
pub struct ParseArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,
}

/// Entry point for the `parse` subcommand.
pub fn run(args: &ParseArgs) -> CliResult<()> {
    let prog = load_program(&args.input)?;

    let funcs = prog
        .items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count();

    println!(
        "parsed OK: {} top-level items, {} functions",
        prog.items.len(),
        funcs
    );
    Ok(())
}